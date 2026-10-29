//! Mutants: small, deliberate changes to code that a good test suite should
//! notice. Each mutant replaces one byte range of the source with other text,
//! so it can be applied on its own or compiled in next to the others.
//!
//! docs/mutation/mutators.md lists the mutators. A mutator proposes a change
//! and the expression or statement its switch goes around (its *site*); each
//! language file then moves the site where a switch is valid, or drops the
//! change when there is no such place.

mod csharp;
mod literal;
mod pattern;
mod rust;
mod suppress;
mod typescript;

use serde::Serialize;
use tree_sitter::Node;

use crate::{Error, Language};

pub use suppress::Suppression;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mutant {
    /// Position in the file's list of mutants, from 0.
    pub id: usize,
    /// One of the names in [`MUTATORS`].
    pub mutator: &'static str,
    pub start_byte: usize,
    pub end_byte: usize,
    /// 1-based.
    pub line: usize,
    pub column: usize,
    /// Where the replaced text ends, 1-based and exclusive.
    pub end_line: usize,
    pub end_column: usize,
    pub original: String,
    pub replacement: String,
    /// The expression or statement the mutant's switch goes around.
    pub site_start: usize,
    pub site_end: usize,
    /// The site is a statement, switched with `if`/`else` instead of `?:`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub statement: bool,
    /// The mutant only changes the value of the condition at its site, and
    /// is switched by combining that value with the switch (the condition
    /// is still evaluated once, with what it declares).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
    /// The site holds a string spanning lines or a preprocessor line, so the
    /// mutated copy keeps its line breaks (C#: between `#line` directives
    /// that keep every line's number; TypeScript: as `\n` in the template).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub multiline: bool,
    /// What the site needs in front of it to be an expression, in both arms
    /// of the switch: `new int[]` for the `{ 1, 2 }` of `int[] a = { 1, 2 }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Why the mutant is not tested: a `dgf-disable` comment, an ignored
    /// method, `[ExcludeFromCodeCoverage]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignored: Option<String>,
    /// A comparison of dates or times: a surviving boundary mutant here is
    /// the off-by-one-day bug, and only a test that pins the boundary may
    /// excuse it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub boundary_risk: bool,
}

impl Mutant {
    /// The source with only this mutant applied.
    pub fn apply(&self, source: &str) -> String {
        let mut text = String::with_capacity(source.len() + self.replacement.len());
        text.push_str(&source[..self.start_byte]);
        text.push_str(&self.replacement);
        text.push_str(&source[self.end_byte..]);
        text
    }
}

/// What a condition-value mutant makes of its condition `c`: `false` is
/// switched as `(c) && !on`, `true` as `(c) || on`, `negated` as `(c) != on`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Condition {
    False,
    True,
    Negated,
}

/// How many mutators run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Basic,
    #[default]
    Standard,
    Advanced,
}

impl std::str::FromStr for Level {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text.to_ascii_lowercase().as_str() {
            "basic" => Ok(Level::Basic),
            "standard" => Ok(Level::Standard),
            "advanced" => Ok(Level::Advanced),
            _ => Err(format!(
                "unknown mutation level {text:?}: use basic, standard or advanced"
            )),
        }
    }
}

/// One kind of mutation.
#[derive(Debug, Serialize)]
pub struct MutatorInfo {
    pub name: &'static str,
    pub level: Level,
    pub example: &'static str,
}

const fn info(name: &'static str, level: Level, example: &'static str) -> MutatorInfo {
    MutatorInfo {
        name,
        level,
        example,
    }
}

/// Every mutator.
pub const MUTATORS: &[MutatorInfo] = &[
    info("arithmetic", Level::Basic, "a + b -> a - b"),
    info("relational", Level::Basic, "a < b -> a <= b, a > b"),
    info(
        "equality",
        Level::Basic,
        "a == b -> a != b; x is T -> x is not T",
    ),
    info("logical", Level::Basic, "a && b -> a || b"),
    info("bitwise", Level::Basic, "a & b -> a | b; a << 1 -> a >> 1"),
    info("block", Level::Basic, "{ ... } -> {}"),
    info("nullcoalescing", Level::Basic, "a ?? b -> a, b, b ?? a"),
    info("statement", Level::Standard, "Save(); -> ;"),
    info("boolean", Level::Standard, "true -> false"),
    info("negation", Level::Standard, "!a -> a"),
    info("negate", Level::Standard, "if (a) -> if (!(a))"),
    info("condition", Level::Standard, "if (a) -> if (false)"),
    info("unary", Level::Standard, "-a -> +a"),
    info("update", Level::Standard, "i++ -> i--"),
    info("assignment", Level::Standard, "a += b -> a -= b"),
    info("checked", Level::Standard, "checked(a + b) -> a + b"),
    info("linq", Level::Standard, "xs.First() -> xs.FirstOrDefault()"),
    info("string", Level::Standard, "\"a\" -> \"\""),
    info(
        "initializer",
        Level::Standard,
        "new T { A = 1 } -> new T { }",
    ),
    info(
        "conditional",
        Level::Standard,
        "c ? a : b -> (true ? a : b)",
    ),
    info("array", Level::Standard, "[a, b] -> []"),
    info("object", Level::Standard, "{ a: 1 } -> {}"),
    info("arrow", Level::Standard, "() => a -> () => undefined"),
    info("optional", Level::Standard, "a?.b -> a.b"),
    info("method", Level::Standard, "s.trim() -> s"),
    info("call", Level::Standard, "save(); -> ;"),
    info("math", Level::Advanced, "Math.Floor(x) -> Math.Ceiling(x)"),
    info(
        "stringmethod",
        Level::Advanced,
        "s.StartsWith(x) -> s.EndsWith(x)",
    ),
    info("collection", Level::Advanced, "[a, b] -> []"),
    info(
        "regex",
        Level::Advanced,
        r#"new Regex("^a+$") -> new Regex("a+$"), /^a+$/ -> /^a$/"#,
    ),
];

/// Mutators whose level differs in TypeScript and JavaScript: regex
/// literals are common there, and cheap to test.
const JAVASCRIPT_LEVELS: &[(&str, Level)] = &[("regex", Level::Standard)];

/// The mutator called `name`.
pub fn mutator(name: &str) -> Option<&'static MutatorInfo> {
    MUTATORS.iter().find(|m| m.name == name)
}

/// Whether `name` (any case, or `all`) refers to the mutator `mutator`.
pub fn names(name: &str, mutator: &str) -> bool {
    let name = name.trim();
    name.eq_ignore_ascii_case("all") || name.eq_ignore_ascii_case(mutator)
}

/// Whether `name` refers to any mutator.
pub fn is_known(name: &str) -> bool {
    MUTATORS.iter().any(|m| names(name, m.name))
}

/// What to mutate.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub level: Level,
    /// Mutator names not to make at all.
    pub ignore_mutators: Vec<String>,
    /// Calls left alone with everything inside them: `*Log*`, `ToString`,
    /// `Console.WriteLine`.
    pub ignore_methods: Vec<String>,
}

/// A file's mutants and the `dgf-disable` comments in it.
#[derive(Debug, Default)]
pub struct Mutations {
    pub mutants: Vec<Mutant>,
    pub suppressions: Vec<Suppression>,
}

/// Every mutant in `source` with the default settings, in source order.
pub fn mutants(source: &str, language: Language) -> Result<Vec<Mutant>, Error> {
    Ok(mutate(source, language, &Settings::default())?.mutants)
}

/// Every mutant in `source`, in source order. Mutants a comment, an ignored
/// method or an attribute turns off are kept with `ignored` set.
pub fn mutate(source: &str, language: Language, settings: &Settings) -> Result<Mutations, Error> {
    let tree = crate::parse(source, language, "<memory>")?;
    let src = source.as_bytes();
    let rules = rules(language);
    let mut found = Vec::new();
    walk(tree.root_node(), &mut |node| {
        if node.is_named() && !node.has_error() {
            rules.collect(node, src, &mut found);
        }
    });
    found.retain(|f| wanted(f.mutator, settings, language));
    let methods = MethodFilter::new(&settings.ignore_methods)?;
    let mut mutants: Vec<Mutant> = found
        .iter()
        .filter_map(|f| {
            let site = rules.place(f, src)?;
            let mut mutant = f.mutant(site, src);
            mutant.ignored = rules.ignored(f, src, &methods);
            mutant.boundary_risk = rules.boundary_risk(f, src);
            Some(mutant)
        })
        .collect();
    drop_crossing(&mut mutants);
    let suppressions = suppress::apply(&tree, source, &mut mutants)?;
    drop_covering(&mut mutants, rules.drops_when_covering());
    mutants.sort_by_key(|m| (m.start_byte, m.end_byte));
    for (id, mutant) in mutants.iter_mut().enumerate() {
        mutant.id = id;
    }
    Ok(Mutations {
        mutants,
        suppressions,
    })
}

fn wanted(mutator: &str, settings: &Settings, language: Language) -> bool {
    let javascript = !matches!(language, Language::CSharp | Language::Rust);
    let own = JAVASCRIPT_LEVELS
        .iter()
        .find(|(name, _)| javascript && *name == mutator)
        .map(|(_, level)| *level);
    let level = own.unwrap_or_else(|| self::mutator(mutator).map_or(Level::Basic, |m| m.level));
    level <= settings.level && !settings.ignore_mutators.iter().any(|n| names(n, mutator))
}

/// Calls each node in `root`'s tree, parents before children.
fn walk<'t>(root: Node<'t>, visit: &mut dyn FnMut(Node<'t>)) {
    let mut cursor = root.walk();
    loop {
        visit(cursor.node());
        if cursor.goto_first_child() || cursor.goto_next_sibling() {
            continue;
        }
        loop {
            if !cursor.goto_parent() {
                return;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// What one language contributes: which nodes make which mutants, where
/// their switches can go, and which are turned off.
trait Rules {
    fn collect<'t>(&self, node: Node<'t>, src: &[u8], found: &mut Vec<Found<'t>>);
    /// The site, adjusted to a place that takes a switch; `None` drops the mutant.
    fn place(&self, found: &Found, src: &[u8]) -> Option<Site>;
    fn ignored(&self, found: &Found, src: &[u8], methods: &MethodFilter) -> Option<String>;
    /// Whether the mutant moves a date or time boundary.
    fn boundary_risk(&self, _found: &Found, _src: &[u8]) -> bool {
        false
    }
    /// The mutator that is only worth a test when no other active mutant is
    /// inside it.
    fn drops_when_covering(&self) -> &'static str {
        "block"
    }
}

fn rules(language: Language) -> &'static dyn Rules {
    match language {
        Language::CSharp => &csharp::CSharp,
        Language::Rust => &rust::Rust,
        _ => &typescript::TypeScript,
    }
}

/// A change a mutator proposes, before it is placed.
struct Found<'t> {
    mutator: &'static str,
    /// What the change is about: suppressions,
    /// ignored methods and placement look at it.
    node: Node<'t>,
    /// The bytes replaced.
    start: usize,
    end: usize,
    replacement: String,
    /// Where the switch goes before placement moves it.
    site: Site,
    /// The node `site` belongs to.
    site_node: Node<'t>,
    /// See [`Mutant::prefix`].
    prefix: Option<String>,
}

/// Where a switch goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Site {
    start: usize,
    end: usize,
    statement: bool,
    condition: Option<Condition>,
    multiline: bool,
}

impl Site {
    fn of(node: Node) -> Site {
        Site {
            start: node.start_byte(),
            end: node.end_byte(),
            statement: false,
            condition: None,
            multiline: false,
        }
    }

    fn statement(node: Node) -> Site {
        Site {
            statement: true,
            ..Site::of(node)
        }
    }
}

impl<'t> Found<'t> {
    /// A change of `node` itself, switched where `node` is.
    fn new(mutator: &'static str, node: Node<'t>, replacement: impl Into<String>) -> Self {
        Found::part(mutator, node, node, replacement)
    }

    /// A change of `part` (an operator, a name) inside `node`, switched where
    /// `node` is.
    fn part(
        mutator: &'static str,
        node: Node<'t>,
        part: Node<'t>,
        replacement: impl Into<String>,
    ) -> Self {
        Found {
            mutator,
            node,
            start: part.start_byte(),
            end: part.end_byte(),
            replacement: replacement.into(),
            site: Site::of(node),
            site_node: node,
            prefix: None,
        }
    }

    /// Switched as a statement.
    fn switched_as_statement(mut self) -> Self {
        self.site.statement = true;
        self
    }

    fn mutant(&self, site: Site, src: &[u8]) -> Mutant {
        let (line, column) = position(src, self.start);
        let (end_line, end_column) = position(src, self.end);
        Mutant {
            id: 0,
            mutator: self.mutator,
            start_byte: self.start,
            end_byte: self.end,
            line,
            column,
            end_line,
            end_column,
            original: String::from_utf8_lossy(&src[self.start..self.end]).into_owned(),
            replacement: self.replacement.clone(),
            site_start: site.start,
            site_end: site.end,
            statement: site.statement,
            condition: site.condition,
            multiline: site.multiline,
            prefix: self.prefix.clone(),
            ignored: None,
            boundary_risk: false,
        }
    }
}

/// 1-based line and column (in bytes) of `byte`.
fn position(src: &[u8], byte: usize) -> (usize, usize) {
    let before = &src[..byte];
    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let column = byte
        - before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |p| p + 1)
        + 1;
    (line, column)
}

fn text<'s>(node: Node, src: &'s [u8]) -> &'s str {
    node.utf8_text(src).unwrap_or_default()
}

/// Two switches can nest but not cross: a mutant whose site overlaps
/// another site only in part is dropped.
fn drop_crossing(mutants: &mut Vec<Mutant>) {
    let sites: Vec<(usize, usize)> = mutants.iter().map(|m| (m.site_start, m.site_end)).collect();
    let crosses = |a: (usize, usize), b: (usize, usize)| {
        (a.0 < b.0 && b.0 < a.1 && a.1 < b.1) || (b.0 < a.0 && a.0 < b.1 && b.1 < a.1)
    };
    mutants.retain(|m| {
        let own = (m.site_start, m.site_end);
        !sites.iter().any(|&other| crosses(own, other))
    });
}

/// Removing a block (`mutator`) is only worth a
/// test when no other active mutant is inside it.
fn drop_covering(mutants: &mut Vec<Mutant>, mutator: &str) {
    let active: Vec<(usize, usize, &'static str)> = mutants
        .iter()
        .filter(|m| m.ignored.is_none())
        .map(|m| (m.start_byte, m.end_byte, m.mutator))
        .collect();
    mutants.retain(|m| {
        m.mutator != mutator
            || !active.iter().any(|&(start, end, other)| {
                (other != mutator || (start, end) != (m.start_byte, m.end_byte))
                    && m.start_byte <= start
                    && end <= m.end_byte
            })
    });
}

/// `ignore_methods`: `*` stays within one name
/// part, any qualifier may come first, generic arguments are optional, and
/// case does not matter.
pub struct MethodFilter {
    patterns: Vec<regex::Regex>,
}

impl MethodFilter {
    pub fn new(patterns: &[String]) -> Result<MethodFilter, Error> {
        let patterns = patterns
            .iter()
            .map(|p| {
                let body = regex::escape(p.trim()).replace(r"\*", "[^.]*");
                regex::RegexBuilder::new(&format!(r"^(?:[^.]*\.)*{body}(<[^>]*>)?$"))
                    .case_insensitive(true)
                    .build()
                    .map_err(|err| Error::Settings(format!("ignore_methods {p:?}: {err}")))
            })
            .collect::<Result<_, _>>()?;
        Ok(MethodFilter { patterns })
    }

    fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Whether a call written `name` (no spaces or comments) is ignored.
    fn matches(&self, name: &str) -> bool {
        self.patterns.iter().any(|p| p.is_match(name))
    }
}

/// `node`'s text without whitespace and comments, as Roslyn prints a node
/// without trivia.
fn compact(node: Node, src: &[u8]) -> String {
    let mut out = String::new();
    walk(node, &mut |n| {
        if n.child_count() == 0 && !n.kind().contains("comment") {
            out.push_str(text(n, src));
        }
    });
    out
}

/// Rewrites `source` so that every given mutant can be switched on while the
/// program runs: each mutated expression becomes
/// `(SWITCH ? (mutated) : (original))` and each mutated statement
/// `{if (SWITCH) {mutated} else {original}}`, nested where sites nest, with
/// `switch(id)` giving the condition. Only one mutant is meant to be active at
/// a time, so the mutated branch uses the plain original text. That copy goes
/// on one line, with comments blanked, so every line keeps its number for
/// compiler errors.
pub fn schemata(
    source: &str,
    language: Language,
    mutants: &[&Mutant],
    switch: &dyn Fn(usize) -> String,
) -> String {
    let mut sites: Vec<(usize, usize)> =
        mutants.iter().map(|m| (m.site_start, m.site_end)).collect();
    sites.sort_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
    sites.dedup();
    let blank = without_comments(source, language);
    let rewrite = Rewrite {
        source,
        blank: &blank,
        mutants,
        switch,
        csharp: language == Language::CSharp,
    };
    rewrite.render(0, source.len(), &sites)
}

/// `source` ready to be put on one line, byte for byte the same length:
/// comments become spaces, and in JavaScript the line break after a
/// statement that ends without `;` becomes one, since nothing else would
/// end it on a single line.
fn without_comments(source: &str, language: Language) -> String {
    let Ok(tree) = crate::parse(source, language, "<memory>") else {
        return source.to_string();
    };
    let mut bytes = source.as_bytes().to_vec();
    let javascript = !matches!(language, Language::CSharp | Language::Rust);
    let mut open_ends = Vec::new();
    let mut templates = Vec::new();
    walk(tree.root_node(), &mut |node| {
        if node.kind().contains("comment") {
            blank(&mut bytes[node.start_byte()..node.end_byte()]);
        } else if javascript {
            line_ends(node, &mut open_ends, &mut templates);
        }
    });
    for template in templates {
        mark_line_breaks(&mut bytes, template);
    }
    for end in open_ends {
        let rest = &bytes[end..];
        let next = rest
            .iter()
            .position(|b| !b.is_ascii_whitespace() || matches!(b, b'\n' | b'\r'));
        if let Some(offset) = next.filter(|&o| matches!(rest[o], b'\n' | b'\r')) {
            bytes[end + offset] = b';';
        }
    }
    // Only ASCII bytes were replaced, by ASCII bytes.
    String::from_utf8(bytes).unwrap_or_else(|_| source.to_string())
}

/// Where a JavaScript line break matters on one line: a statement that ends
/// without `;`, and an untagged template string.
fn line_ends<'t>(node: Node<'t>, open_ends: &mut Vec<usize>, templates: &mut Vec<Node<'t>>) {
    if needs_semicolon(node) {
        open_ends.push(node.end_byte());
    } else if node.kind() == "template_string" && !is_tagged(node) {
        templates.push(node);
    }
}

/// A line break inside a JavaScript template string, as `without_comments`
/// marks it: `\n` (and a lone `\r`) become [`TEMPLATE_LF`], the `\r` of
/// `\r\n` becomes [`TEMPLATE_CR`]. A template's line break means `\n`
/// whatever the file uses, so a copy writes it as that escape.
const TEMPLATE_LF: char = '\u{1}';
const TEMPLATE_CR: char = '\u{2}';

/// `` String.raw`...` `` sees its text as written: a line break cannot
/// become `\n` there.
pub(super) fn is_tagged(template: Node) -> bool {
    template.parent().is_some_and(|p| {
        p.kind() == "call_expression" && p.child_by_field_name("arguments") == Some(template)
    })
}

/// Marks the line breaks of `template`'s text, not of its `${...}` parts.
fn mark_line_breaks(bytes: &mut [u8], template: Node) {
    let mut cursor = template.walk();
    let holes: Vec<(usize, usize)> = template
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "template_substitution")
        .map(|c| (c.start_byte(), c.end_byte()))
        .collect();
    for i in template.start_byte()..template.end_byte() {
        if holes.iter().any(|&(s, e)| (s..e).contains(&i)) {
            continue;
        }
        let crlf = bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n');
        bytes[i] = match bytes[i] {
            b'\r' if crlf => TEMPLATE_CR as u8,
            b'\r' | b'\n' => TEMPLATE_LF as u8,
            other => other,
        };
    }
}

/// JavaScript statements that end with `;`, written or inserted.
const ENDS_WITH_SEMICOLON: &[&str] = &[
    "expression_statement",
    "lexical_declaration",
    "variable_declaration",
    "return_statement",
    "throw_statement",
    "break_statement",
    "continue_statement",
    "debugger_statement",
    "do_statement",
    "public_field_definition",
];

/// Spaces for every byte but line breaks.
fn blank(bytes: &mut [u8]) {
    for b in bytes {
        if *b != b'\n' && *b != b'\r' {
            *b = b' ';
        }
    }
}

fn needs_semicolon(node: Node) -> bool {
    ENDS_WITH_SEMICOLON.contains(&node.kind()) && !ends_with_semicolon(node)
}

fn ends_with_semicolon(node: Node) -> bool {
    node.child(node.child_count().saturating_sub(1))
        .is_some_and(|last| last.kind() == ";" && last.end_byte() > last.start_byte())
}

impl Rewrite<'_> {
    /// The mutated copy next to the original: `?:` for an expression,
    /// `if`/`else` for a statement.
    fn copied(&self, m: &Mutant, start: usize, end: usize, switch: &str, text: &str) -> String {
        let copy = format!(
            "{}{}{}{}",
            m.prefix
                .as_deref()
                .map_or(String::new(), |p| format!("{p} ")),
            &self.blank[start..m.start_byte],
            m.replacement,
            &self.blank[m.end_byte..end]
        );
        let mutated = self.on_lines(m, start, copy);
        if m.statement {
            format!("{{if ({switch}) {{{mutated}}} else {{{text}}}}}")
        } else {
            format!("({switch} ? ({mutated}) : ({text}))")
        }
    }

    /// The copy on the lines it takes: one, or, for a multi-line C# copy,
    /// its own lines between `#line` directives that give them, and the
    /// original after them, the site's line numbers. In TypeScript a line
    /// break in a template string is marked (see `without_comments`) and
    /// becomes `\n`.
    fn on_lines(&self, m: &Mutant, start: usize, copy: String) -> String {
        if m.multiline && self.csharp {
            let line = position(self.source.as_bytes(), start).0;
            return format!("\n#line {line}\n{copy}\n#line {line}\n");
        }
        copy.replace(TEMPLATE_CR, "")
            .replace(TEMPLATE_LF, "\\n")
            .replace(['\r', '\n'], " ")
    }
}

struct Rewrite<'a> {
    source: &'a str,
    blank: &'a str,
    mutants: &'a [&'a Mutant],
    switch: &'a dyn Fn(usize) -> String,
    csharp: bool,
}

impl Rewrite<'_> {
    /// `start..end` with every top-level site in `sites` wrapped.
    fn render(&self, start: usize, end: usize, sites: &[(usize, usize)]) -> String {
        let mut out = String::new();
        let mut position = start;
        let mut index = 0;
        while index < sites.len() {
            let (site_start, site_end) = sites[index];
            let inner = sites[index + 1..]
                .iter()
                .take_while(|(s, _)| *s < site_end)
                .count();
            out.push_str(&self.source[position..site_start]);
            out.push_str(&self.wrap(site_start, site_end, &sites[index + 1..index + 1 + inner]));
            position = site_end;
            index += 1 + inner;
        }
        out.push_str(&self.source[position..end]);
        out
    }

    fn wrap(&self, start: usize, end: usize, inner: &[(usize, usize)]) -> String {
        let mut text = self.render(start, end, inner);
        let at_site = |m: &&&Mutant| m.site_start == start && m.site_end == end;
        if let Some(prefix) = self
            .mutants
            .iter()
            .filter(at_site)
            .find_map(|m| m.prefix.as_ref())
        {
            text = format!("{prefix} {text}");
        }
        for m in self
            .mutants
            .iter()
            .filter(|m| m.site_start == start && m.site_end == end)
        {
            let switch = (self.switch)(m.id);
            text = match m.condition {
                Some(Condition::False) => format!("(({text}) && !{switch})"),
                Some(Condition::True) => format!("(({text}) || {switch})"),
                Some(Condition::Negated) => format!("(({text}) != {switch})"),
                None => self.copied(m, start, end, &switch, &text),
            };
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_parse() {
        assert_eq!("Basic".parse::<Level>(), Ok(Level::Basic));
        assert_eq!("advanced".parse::<Level>(), Ok(Level::Advanced));
        assert!("complete".parse::<Level>().is_err());
        assert!("full".parse::<Level>().is_err());
        assert!(Level::Basic < Level::Standard);
    }

    #[test]
    fn names_match_in_any_case() {
        assert!(names("relational", "relational"));
        assert!(names("Relational", "relational"));
        assert!(!names("Equality", "relational"));
        assert!(names("all", "block"));
        assert!(!names("logical", "relational"));
        assert!(is_known("Statement") && !is_known("EqualityOperator"));
        assert!(MUTATORS.iter().all(|m| mutator(m.name).is_some()));
    }

    #[test]
    fn method_filter_matches_name_parts() {
        let filter =
            MethodFilter::new(&["*Log*".into(), "ToString".into(), "Console.Write".into()])
                .unwrap();
        assert!(filter.matches("_logger.LogInformation"));
        assert!(filter.matches("Login"));
        assert!(filter.matches("x.ToString"));
        assert!(filter.matches("ToString<int>"));
        assert!(filter.matches("System.Console.Write"));
        assert!(!filter.matches("Catalog.Get"));
        assert!(!filter.matches("x.ToStringy"));
        assert!(!filter.matches("Console.WriteLine"));
    }

    #[test]
    fn position_is_one_based() {
        assert_eq!(position(b"ab\ncd", 4), (2, 2));
        assert_eq!(position(b"ab", 0), (1, 1));
    }

    #[test]
    fn one_line_copies_keep_javascript_statements_apart() {
        let src = "function f(a: number) {
  switch (a) {
    case 1:
      save(a)
      return 1
    case 2:
      return 2
  }
  return 0
}
";
        let mutants = mutants(src, Language::TypeScript).unwrap();
        let emptied = mutants
            .iter()
            .find(|m| m.original == "return 2" && m.replacement == ";")
            .unwrap();
        let text = schemata(src, Language::TypeScript, &[emptied], &|id| {
            format!("on({id})")
        });
        assert!(
            text.contains("save(a);      return 1;    case 2:"),
            "{text}"
        );
        let tree = crate::parse(&text, Language::TypeScript, "test").unwrap();
        assert!(!tree.root_node().has_error(), "{text}");
    }

    #[test]
    fn multi_line_copies_keep_strings_and_line_numbers() {
        let src = "class C { bool M(string s) {\n  return s == @\"a\nb\";\n} }";
        let mutants = mutants(src, Language::CSharp).unwrap();
        let equality = mutants.iter().find(|m| m.replacement == "!=").unwrap();
        assert!(equality.multiline);
        let text = schemata(src, Language::CSharp, &[equality], &|id| format!("S({id})"));
        assert_eq!(
            text,
            "class C { bool M(string s) {\n  return (S(0) ? (\n#line 2\ns != @\"a\nb\"\n#line 2\n) : (s == @\"a\nb\"));\n} }"
        );

        let src = "function f(s: string, y: number) {\n  return s === `a\r\n${y}` && y > 0;\n}\n";
        let found = super::mutants(src, Language::TypeScript).unwrap();
        let logical = found.iter().find(|m| m.mutator == "logical").unwrap();
        assert!(logical.multiline);
        let text = schemata(src, Language::TypeScript, &[logical], &|_| "on".into());
        assert!(text.contains("(on ? (s === `a\\n${y}` || y > 0)"), "{text}");
        assert_eq!(text.lines().count(), src.lines().count());
    }

    #[test]
    fn statement_switches_and_comment_free_copies() {
        let src = "class C { void M(int a) {\n  Save(a); // keep\n  a++;\n} }";
        let mutants = mutants(src, Language::CSharp).unwrap();
        let save = mutants.iter().find(|m| m.original == "Save(a);").unwrap();
        assert!(save.statement);
        let text = schemata(src, Language::CSharp, &[save], &|id| format!("S({id})"));
        assert_eq!(text.lines().count(), src.lines().count());
        assert!(
            text.contains("{if (S(0)) {;} else {Save(a);}} // keep"),
            "{text}"
        );
    }
}
