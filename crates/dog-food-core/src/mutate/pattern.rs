//! Mutations of regular expression patterns. .NET patterns get sixteen
//! kinds; JavaScript patterns get the six in `LEVEL_ONE`. A pattern the
//! parser here does not understand gets none, and neither does a mutated
//! pattern that no longer parses (`new Regex` would reject it).

use std::sync::LazyLock;

use regex::Regex;

/// Which regex syntax a pattern is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    DotNet,
    /// `unicode`: the `u` flag, which gives `\p{...}` its meaning.
    JavaScript {
        unicode: bool,
    },
}

/// One mutated pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutation {
    pub pattern: String,
    /// What changed: "anchor removal", "quantifier removal".
    pub what: &'static str,
}

/// Every mutation of `pattern`, in pattern order, without repeats.
pub fn mutations(pattern: &str, flavor: Flavor) -> Vec<Mutation> {
    let Some(parts) = parse(pattern, flavor) else {
        return Vec::new();
    };
    let mut out: Vec<Mutation> = Vec::new();
    for edit in edits(&parts, pattern, flavor) {
        let changed = edit.apply(pattern);
        if is_new(&out, pattern, &changed) && parse(&changed, flavor).is_some() {
            out.push(Mutation {
                pattern: changed,
                what: edit.what,
            });
        }
    }
    out
}

fn is_new(out: &[Mutation], pattern: &str, changed: &str) -> bool {
    changed != pattern && !out.iter().any(|m| m.pattern == changed)
}

/// `start..end` of the pattern becomes `text`.
struct Edit {
    start: usize,
    end: usize,
    text: String,
    what: &'static str,
}

impl Edit {
    fn new(start: usize, end: usize, text: impl Into<String>, what: &'static str) -> Edit {
        Edit {
            start,
            end,
            text: text.into(),
            what,
        }
    }

    fn apply(&self, pattern: &str) -> String {
        format!(
            "{}{}{}",
            &pattern[..self.start],
            self.text,
            &pattern[self.end..]
        )
    }
}

// ---------------------------------------------------------------------------
// What a pattern is made of, as far as the mutations need it. Positions are
// byte offsets into the pattern.

#[derive(Default)]
struct Parts {
    anchors: Vec<Anchor>,
    classes: Vec<Class>,
    /// The `\` of `\d`, `\w`, `\s` and their negations.
    shorthands: Vec<usize>,
    /// The `p` or `P` of `\p{...}`.
    unicode: Vec<usize>,
    quantifiers: Vec<Quantifier>,
    /// The `(` of a capturing group without a name.
    groups: Vec<usize>,
    /// The `=` or `!` of a lookaround.
    lookarounds: Vec<usize>,
    ranges: Vec<[End; 2]>,
    captures: usize,
    names: Vec<String>,
    references: Vec<Reference>,
}

struct Anchor {
    start: usize,
    end: usize,
    /// `^` or `$`, the only anchors removed.
    line: bool,
}

struct Class {
    open: usize,
    end: usize,
    negated: bool,
    children: Vec<Child>,
    /// `-[...]` (.NET): from the `-` to the end of the inner class.
    subtraction: Option<(usize, usize)>,
}

struct Child {
    start: usize,
    end: usize,
    shorthand: Option<char>,
}

struct Quantifier {
    start: usize,
    /// The end of the quantifier, before a lazy `?`.
    end: usize,
    lazy: bool,
    count: Count,
}

#[derive(Clone, Copy)]
enum Count {
    Star,
    Plus,
    Question,
    Exactly,
    AtLeast(u32),
    Between(u32, u32),
}

/// One end of a range in a class: `a` and `z` of `[a-z]`.
#[derive(Clone, Copy)]
struct End {
    start: usize,
    end: usize,
    code: u32,
    form: Form,
}

/// How a character is written, which says how a changed one is written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    Plain,
    Hex,
    Unicode,
    Other,
}

enum Reference {
    Number(usize),
    Name(String),
}

/// A character in a class, or a shorthand.
struct Atom {
    code: Option<u32>,
    form: Form,
    shorthand: Option<char>,
}

impl Atom {
    fn char(code: u32, form: Form) -> Atom {
        Atom {
            code: Some(code),
            form,
            shorthand: None,
        }
    }

    fn other() -> Atom {
        Atom {
            code: None,
            form: Form::Other,
            shorthand: None,
        }
    }
}

fn parse(pattern: &str, flavor: Flavor) -> Option<Parts> {
    let mut parser = Parser {
        text: pattern,
        pos: 0,
        flavor,
        depth: 0,
        parts: Parts::default(),
    };
    parser.sequence()?;
    let parts = parser.parts;
    parts
        .references
        .iter()
        .all(|r| resolves(r, &parts))
        .then_some(parts)
}

/// A back reference to a group that does not exist is an error in both
/// flavors (`(?:a)\1` after a group became non-capturing).
fn resolves(reference: &Reference, parts: &Parts) -> bool {
    match reference {
        Reference::Number(n) => *n > 9 || *n <= parts.captures,
        Reference::Name(name) => {
            parts.names.contains(name) || name.parse::<usize>().is_ok_and(|n| n <= parts.captures)
        }
    }
}

struct Parser<'p> {
    text: &'p str,
    pos: usize,
    flavor: Flavor,
    depth: usize,
    parts: Parts,
}

/// `{n}`, `{n,}` and `{n,m}`.
static COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\{(\d+)(,(\d*))?\}").expect("valid count regex"));
/// A group name and, in .NET, a balancing group's second name.
static GROUP_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:<([A-Za-z_]\w*)(?:-\w+)?>|'([A-Za-z_]\w*)(?:-\w+)?')")
        .expect("valid name regex")
});
/// Inline options: `(?i)`, `(?i-s:...)`.
static OPTIONS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[imnsx]*(?:-[imnsx]*)?").expect("valid options regex"));
/// `\k<name>`.
static NAMED_REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^k<(\w+)>").expect("valid reference regex"));
/// .NET's Unicode categories; `\p{IsGreek}`-style block names are checked
/// by their `Is`.
const CATEGORIES: &[&str] = &[
    "L", "Lu", "Ll", "Lt", "Lm", "Lo", "M", "Mn", "Mc", "Me", "N", "Nd", "Nl", "No", "Z", "Zs",
    "Zl", "Zp", "C", "Cc", "Cf", "Cs", "Co", "Cn", "P", "Pc", "Pd", "Ps", "Pe", "Pi", "Pf", "Po",
    "S", "Sm", "Sc", "Sk", "So",
];
/// Escapes for one character, with its code.
const ESCAPES: &[(char, u32)] = &[
    ('t', 9),
    ('n', 10),
    ('v', 11),
    ('f', 12),
    ('r', 13),
    ('a', 7),
    ('e', 27),
    ('0', 0),
];
const LOOKAROUNDS: &[&str] = &["=", "!", "<=", "<!"];

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        let matches = self.peek() == Some(c);
        if matches {
            self.pos += c.len_utf8();
        }
        matches
    }

    fn dotnet(&self) -> bool {
        self.flavor == Flavor::DotNet
    }

    /// Alternatives up to the end, or to the `)` of the open group.
    fn sequence(&mut self) -> Option<()> {
        loop {
            match self.peek() {
                None => return (self.depth == 0).then_some(()),
                Some(')') => return (self.depth > 0).then_some(()),
                Some('|') => self.pos += 1,
                Some(_) => self.term()?,
            }
        }
    }

    fn term(&mut self) -> Option<()> {
        let quantifiable = self.atom()?;
        self.quantifier(quantifiable)
    }

    /// One element; whether a quantifier may follow it.
    fn atom(&mut self) -> Option<bool> {
        match self.peek()? {
            '(' => self.group().map(|()| true),
            '[' => self.class().map(|()| true),
            '\\' => self.escape(),
            '^' | '$' => {
                self.anchor(1, true);
                Some(false)
            }
            c => self.literal(c),
        }
    }

    fn literal(&mut self, c: char) -> Option<bool> {
        if matches!(c, '*' | '+' | '?') || self.count().is_some() {
            return None;
        }
        self.bump();
        Some(true)
    }

    fn anchor(&mut self, length: usize, line: bool) {
        self.parts.anchors.push(Anchor {
            start: self.pos,
            end: self.pos + length,
            line,
        });
        self.pos += length;
    }

    fn quantifier(&mut self, quantifiable: bool) -> Option<()> {
        let start = self.pos;
        let Some((count, length)) = self.quantity() else {
            return Some(());
        };
        if !quantifiable {
            return None;
        }
        self.pos += length;
        let end = self.pos;
        let lazy = self.eat('?');
        if self.quantity().is_some() {
            return None;
        }
        self.parts.quantifiers.push(Quantifier {
            start,
            end,
            lazy,
            count,
        });
        Some(())
    }

    fn quantity(&self) -> Option<(Count, usize)> {
        match self.peek()? {
            '*' => Some((Count::Star, 1)),
            '+' => Some((Count::Plus, 1)),
            '?' => Some((Count::Question, 1)),
            '{' => self.count(),
            _ => None,
        }
    }

    /// A `{n,m}` quantifier here; `{` alone is a literal.
    fn count(&self) -> Option<(Count, usize)> {
        let captures = COUNT.captures(self.rest())?;
        let n: u32 = captures[1].parse().ok()?;
        let count = match (captures.get(2), captures.get(3).map(|m| m.as_str())) {
            (None, _) => Count::Exactly,
            (Some(_), None | Some("")) => Count::AtLeast(n),
            (Some(_), Some(m)) => Count::Between(n, m.parse().ok().filter(|m| *m >= n)?),
        };
        Some((count, captures[0].len()))
    }

    fn group(&mut self) -> Option<()> {
        let open = self.pos;
        self.pos += 1;
        let body = if self.eat('?') {
            self.group_kind()?
        } else {
            self.parts.groups.push(open);
            self.parts.captures += 1;
            true
        };
        if body {
            self.depth += 1;
            self.sequence()?;
            self.depth -= 1;
        }
        self.eat(')').then_some(())
    }

    /// After `(?`: what kind of group, and whether it has a body.
    fn group_kind(&mut self) -> Option<bool> {
        if let Some(prefix) = LOOKAROUNDS.iter().find(|p| self.rest().starts_with(**p)) {
            self.parts.lookarounds.push(self.pos + prefix.len() - 1);
            self.pos += prefix.len();
            return Some(true);
        }
        if self.eat(':') || (self.dotnet() && self.eat('>')) {
            return Some(true);
        }
        if let Some(captures) = GROUP_NAME.captures(self.rest()) {
            let name = captures.get(1).or(captures.get(2))?.as_str().to_string();
            self.pos += captures[0].len();
            self.parts.names.push(name);
            self.parts.captures += 1;
            return Some(true);
        }
        self.options()
    }

    /// Inline options (`(?i)`, `(?i:...)`) and .NET's `(?#comment)`.
    fn options(&mut self) -> Option<bool> {
        if self.dotnet() && self.peek() == Some('#') {
            self.pos += self.rest().find(')')?;
            return Some(false);
        }
        self.pos += OPTIONS.find(self.rest())?.end();
        match self.peek()? {
            ':' => {
                self.pos += 1;
                Some(true)
            }
            ')' if self.dotnet() => Some(false),
            _ => None,
        }
    }

    fn class(&mut self) -> Option<()> {
        let open = self.pos;
        self.pos += 1;
        let negated = self.eat('^');
        let mut class = Class {
            open,
            end: 0,
            negated,
            children: Vec::new(),
            subtraction: None,
        };
        while !self.class_closes(class.children.is_empty()) {
            if self.subtraction_starts() {
                class.subtraction = Some(self.subtraction()?);
                break;
            }
            class.children.push(self.class_child()?);
        }
        self.eat(']').then_some(())?;
        class.end = self.pos;
        self.parts.classes.push(class);
        Some(())
    }

    /// .NET reads a `]` first in a class as a literal; JavaScript's `[]`
    /// is an empty class.
    fn class_closes(&self, first: bool) -> bool {
        self.peek() == Some(']') && !(first && self.dotnet())
    }

    fn subtraction_starts(&self) -> bool {
        self.dotnet() && self.rest().starts_with("-[")
    }

    fn subtraction(&mut self) -> Option<(usize, usize)> {
        let start = self.pos;
        self.pos += 1;
        self.class()?;
        Some((start, self.pos))
    }

    fn class_child(&mut self) -> Option<Child> {
        let start = self.pos;
        let first = self.class_atom()?;
        if !self.range_follows() {
            return Some(Child {
                start,
                end: self.pos,
                shorthand: first.shorthand,
            });
        }
        self.range(start, first)
    }

    fn range_follows(&self) -> bool {
        self.peek() == Some('-')
            && !matches!(self.peek_at(1), None | Some(']'))
            && !self.subtraction_starts()
    }

    /// `a-z`, from its `-` on. A shorthand cannot end a range.
    fn range(&mut self, start: usize, first: Atom) -> Option<Child> {
        let first_end = self.pos;
        self.pos += 1;
        let second_start = self.pos;
        let second = self.class_atom()?;
        let (low, high) = (first.code?, second.code?);
        if low > high {
            return None;
        }
        let end = |start, end, code, form| End {
            start,
            end,
            code,
            form,
        };
        self.parts.ranges.push([
            end(start, first_end, low, first.form),
            end(second_start, self.pos, high, second.form),
        ]);
        Some(Child {
            start,
            end: self.pos,
            shorthand: None,
        })
    }

    fn class_atom(&mut self) -> Option<Atom> {
        let c = self.bump()?;
        if c != '\\' {
            return Some(Atom::char(c as u32, Form::Plain));
        }
        self.escaped(true)
    }

    /// An escape outside a class; whether a quantifier may follow it.
    fn escape(&mut self) -> Option<bool> {
        let at = self.pos;
        self.pos += 1;
        let c = self.peek()?;
        if self.is_anchor_escape(c) {
            self.pos = at;
            self.anchor(2, false);
            return Some(false);
        }
        if c.is_ascii_digit() && c != '0' {
            return self.backreference();
        }
        if c == 'k' && self.peek_at(1) == Some('<') {
            return self.named_reference();
        }
        self.escaped(false).map(|_| true)
    }

    fn is_anchor_escape(&self, c: char) -> bool {
        matches!(c, 'b' | 'B') || (self.dotnet() && matches!(c, 'A' | 'Z' | 'z' | 'G'))
    }

    fn backreference(&mut self) -> Option<bool> {
        let digits = self.rest().len()
            - self
                .rest()
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .len();
        let number = self.rest()[..digits].parse().ok()?;
        self.pos += digits;
        self.parts.references.push(Reference::Number(number));
        Some(true)
    }

    fn named_reference(&mut self) -> Option<bool> {
        let captures = NAMED_REFERENCE.captures(self.rest())?;
        let (name, length) = (captures[1].to_string(), captures[0].len());
        self.parts.references.push(Reference::Name(name));
        self.pos += length;
        Some(true)
    }

    /// What follows a `\` that is no anchor or back reference.
    fn escaped(&mut self, in_class: bool) -> Option<Atom> {
        let at = self.pos - 1;
        let c = self.peek()?;
        if "dDwWsS".contains(c) {
            self.pos += 1;
            self.parts.shorthands.push(at);
            return Some(Atom {
                shorthand: Some(c),
                ..Atom::other()
            });
        }
        if matches!(c, 'p' | 'P') && self.has_unicode_classes() {
            return self.unicode();
        }
        if matches!(c, 'x' | 'u') {
            return self.hex(c);
        }
        self.pos += c.len_utf8();
        let code = self.escape_code(c, in_class)?;
        Some(Atom::char(code, Form::Other))
    }

    fn has_unicode_classes(&self) -> bool {
        matches!(
            self.flavor,
            Flavor::DotNet | Flavor::JavaScript { unicode: true }
        )
    }

    fn escape_code(&mut self, c: char, in_class: bool) -> Option<u32> {
        if c == 'b' && in_class {
            return Some(8);
        }
        if c == 'c' {
            return self.bump().map(|letter| letter as u32 % 32);
        }
        if let Some(&(_, code)) = ESCAPES.iter().find(|(e, _)| *e == c) {
            return Some(code);
        }
        // .NET rejects an escaped letter or digit it does not know.
        (!c.is_ascii_alphanumeric() || !self.dotnet()).then_some(c as u32)
    }

    /// `\p{L}` or `\P{L}`, from its `p`.
    fn unicode(&mut self) -> Option<Atom> {
        let at = self.pos;
        self.pos += 1;
        let close = self.rest().find('}').filter(|_| self.peek() == Some('{'))?;
        let name = &self.rest()[1..close];
        if !self.is_category(name) {
            return None;
        }
        self.pos += close + 1;
        self.parts.unicode.push(at);
        Some(Atom::other())
    }

    fn is_category(&self, name: &str) -> bool {
        let known =
            CATEGORIES.contains(&name) || name.strip_prefix("Is").is_some_and(|b| !b.is_empty());
        known || (!self.dotnet() && !name.is_empty())
    }

    /// `\xHH`, `\uHHHH`, and JavaScript's `\u{H...}`, from the letter.
    fn hex(&mut self, letter: char) -> Option<Atom> {
        self.pos += 1;
        if letter == 'u' && !self.dotnet() && self.peek() == Some('{') {
            self.pos += self.rest().find('}')? + 1;
            return Some(Atom::other());
        }
        let (digits, form) = if letter == 'x' {
            (2, Form::Hex)
        } else {
            (4, Form::Unicode)
        };
        let text = self.rest().get(..digits)?;
        let code = u32::from_str_radix(text, 16).ok()?;
        self.pos += digits;
        Some(Atom::char(code, form))
    }
}

// ---------------------------------------------------------------------------
// The mutations, as edits of the pattern.

type Generator = fn(&Parts, &str, &mut Vec<Edit>);

/// The set both flavors get.
const LEVEL_ONE: &[Generator] = &[
    anchor_removal,
    class_negation,
    shorthand_negation,
    quantifier_removal,
    unicode_negation,
    lookaround_negation,
];

/// The rest of the .NET set.
const DOTNET_ONLY: &[Generator] = &[
    class_child_removal,
    class_to_any,
    shorthand_nullification,
    shorthand_to_any,
    quantity_change,
    group_to_non_capturing,
    range_change,
    reluctant_addition,
];

fn edits(parts: &Parts, pattern: &str, flavor: Flavor) -> Vec<Edit> {
    let mut edits = Vec::new();
    let more: &[Generator] = if flavor == Flavor::DotNet {
        DOTNET_ONLY
    } else {
        &[]
    };
    for generate in LEVEL_ONE.iter().chain(more) {
        generate(parts, pattern, &mut edits);
    }
    if flavor != Flavor::DotNet {
        edits.retain(|e| e.what != "anchor removal" || is_line_anchor(parts, e.start));
    }
    edits.sort_by_key(|e| e.start);
    edits
}

fn is_line_anchor(parts: &Parts, start: usize) -> bool {
    parts.anchors.iter().any(|a| a.start == start && a.line)
}

fn anchor_removal(parts: &Parts, _: &str, edits: &mut Vec<Edit>) {
    for a in &parts.anchors {
        edits.push(Edit::new(a.start, a.end, "", "anchor removal"));
    }
}

fn class_negation(parts: &Parts, _: &str, edits: &mut Vec<Edit>) {
    for c in &parts.classes {
        let caret = c.open + 1;
        let (end, text) = if c.negated {
            (caret + 1, "")
        } else {
            (caret, "^")
        };
        edits.push(Edit::new(caret, end, text, "character class negation"));
    }
}

fn swapped_case(c: char) -> char {
    if c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c.to_ascii_lowercase()
    }
}

fn letter_at(pattern: &str, at: usize) -> char {
    pattern[at..].chars().next().unwrap_or(' ')
}

fn shorthand_negation(parts: &Parts, pattern: &str, edits: &mut Vec<Edit>) {
    for &at in &parts.shorthands {
        let letter = swapped_case(letter_at(pattern, at + 1));
        edits.push(Edit::new(
            at + 1,
            at + 2,
            letter.to_string(),
            "shorthand negation",
        ));
    }
}

fn quantifier_removal(parts: &Parts, _: &str, edits: &mut Vec<Edit>) {
    for q in &parts.quantifiers {
        let end = q.end + usize::from(q.lazy);
        edits.push(Edit::new(q.start, end, "", "quantifier removal"));
    }
}

fn unicode_negation(parts: &Parts, pattern: &str, edits: &mut Vec<Edit>) {
    for &at in &parts.unicode {
        let letter = swapped_case(letter_at(pattern, at));
        edits.push(Edit::new(
            at,
            at + 1,
            letter.to_string(),
            "unicode class negation",
        ));
    }
}

fn lookaround_negation(parts: &Parts, pattern: &str, edits: &mut Vec<Edit>) {
    for &at in &parts.lookarounds {
        let flipped = if letter_at(pattern, at) == '=' {
            "!"
        } else {
            "="
        };
        edits.push(Edit::new(at, at + 1, flipped, "lookaround negation"));
    }
}

/// A child of a class with more than one is removed; a subtraction is
/// removed, or, when the class has one child, takes its place.
fn class_child_removal(parts: &Parts, pattern: &str, edits: &mut Vec<Edit>) {
    for c in &parts.classes {
        if let Some(subtraction) = c.subtraction {
            subtraction_edits(parts, c, subtraction, pattern, edits);
        }
        if c.children.len() > 1 {
            for child in &c.children {
                edits.push(Edit::new(
                    child.start,
                    child.end,
                    "",
                    "character class child removal",
                ));
            }
        }
    }
}

fn subtraction_edits(
    parts: &Parts,
    class: &Class,
    (start, end): (usize, usize),
    pattern: &str,
    edits: &mut Vec<Edit>,
) {
    edits.push(Edit::new(
        start,
        end,
        "",
        "character class subtraction removal",
    ));
    let inner = parts.classes.iter().find(|c| c.open == start + 1);
    let set = inner.and_then(|c| Some(&pattern[c.children.first()?.start..c.children.last()?.end]));
    if let Some(set) = set.filter(|_| class.children.len() == 1) {
        let caret = if class.negated { "^" } else { "" };
        edits.push(Edit::new(
            class.open,
            class.end,
            format!("[{caret}{set}]"),
            "character class subtraction replacement",
        ));
    }
}

fn class_to_any(parts: &Parts, _: &str, edits: &mut Vec<Edit>) {
    for c in parts.classes.iter().filter(|c| !is_any(c)) {
        edits.push(Edit::new(
            c.open,
            c.end,
            r"[\w\W]",
            "character class to any character",
        ));
    }
}

/// `[\w\W]` already, or `[\W\w]`.
fn is_any(class: &Class) -> bool {
    class.subtraction.is_none()
        && class.children.len() == 2
        && class
            .children
            .iter()
            .all(|c| matches!(c.shorthand, Some('w' | 'W')))
}

fn shorthand_nullification(parts: &Parts, _: &str, edits: &mut Vec<Edit>) {
    for &at in &parts.shorthands {
        edits.push(Edit::new(at, at + 1, "", "shorthand nullification"));
    }
}

fn shorthand_to_any(parts: &Parts, pattern: &str, edits: &mut Vec<Edit>) {
    for &at in &parts.shorthands {
        let letter = letter_at(pattern, at + 1);
        let both = format!(r"[\{letter}\{}]", swapped_case(letter));
        edits.push(Edit::new(at, at + 2, both, "shorthand to any character"));
    }
}

/// `{n,m}` one wider or narrower at each end, `{n,}` one lower or higher;
/// `*`, `+` and `?` as `{0,}`, `{1,}` and `{0,1}` the same way.
fn quantity_change(parts: &Parts, _: &str, edits: &mut Vec<Edit>) {
    for q in &parts.quantifiers {
        for text in quantities(q.count) {
            edits.push(Edit::new(q.start, q.end, text, "quantifier change"));
        }
    }
}

fn quantities(count: Count) -> Vec<String> {
    match count {
        Count::Star => at_least(0),
        Count::Plus => at_least(1),
        Count::Question => between(0, 1),
        Count::Exactly => Vec::new(),
        Count::AtLeast(n) => at_least(i64::from(n)),
        Count::Between(n, m) => between(i64::from(n), i64::from(m)),
    }
}

fn at_least(n: i64) -> Vec<String> {
    [n - 1, n + 1]
        .into_iter()
        .filter(|n| *n >= 0)
        .map(|n| format!("{{{n},}}"))
        .collect()
}

fn between(n: i64, m: i64) -> Vec<String> {
    [(n - 1, m), (n + 1, m), (n, m - 1), (n, m + 1)]
        .into_iter()
        .filter(|&(a, b)| a >= 0 && b >= 0 && a <= b)
        .map(|(a, b)| format!("{{{a},{b}}}"))
        .collect()
}

fn group_to_non_capturing(parts: &Parts, _: &str, edits: &mut Vec<Edit>) {
    for &open in &parts.groups {
        edits.push(Edit::new(
            open + 1,
            open + 1,
            "?:",
            "group to non-capturing",
        ));
    }
}

/// Each end of a range one character lower and higher, while the range
/// stays in order and a plain character stays a letter or digit.
fn range_change(parts: &Parts, _: &str, edits: &mut Vec<Edit>) {
    for range in &parts.ranges {
        for (index, end) in range.iter().enumerate() {
            let other = range[1 - index].code;
            for code in [end.code.checked_sub(1), end.code.checked_add(1)]
                .into_iter()
                .flatten()
            {
                let ordered = if index == 0 {
                    code <= other
                } else {
                    other <= code
                };
                if let Some(text) = written(code, end.form).filter(|_| ordered) {
                    edits.push(Edit::new(
                        end.start,
                        end.end,
                        text,
                        "character class range change",
                    ));
                }
            }
        }
    }
}

/// `code` written in `form`, if it can be.
fn written(code: u32, form: Form) -> Option<String> {
    match form {
        Form::Plain => char::from_u32(code)
            .filter(|c| c.is_alphanumeric())
            .map(String::from),
        Form::Hex => (code <= 0xFF).then(|| format!(r"\x{code:02x}")),
        Form::Unicode => (code <= 0xFFFF).then(|| format!(r"\u{code:04x}")),
        Form::Other => None,
    }
}

fn reluctant_addition(parts: &Parts, _: &str, edits: &mut Vec<Edit>) {
    for q in parts.quantifiers.iter().filter(|q| !q.lazy) {
        edits.push(Edit::new(q.end, q.end, "?", "reluctant quantifier"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(pattern: &str, flavor: Flavor) -> Vec<String> {
        mutations(pattern, flavor)
            .into_iter()
            .map(|m| m.pattern)
            .collect()
    }

    const JS: Flavor = Flavor::JavaScript { unicode: false };

    #[test]
    fn dotnet_examples() {
        // "^abc" loses its anchor, "abcX?" gets five
        // mutants, "abc[XY]" four, "abc\d" three.
        assert_eq!(patterns("^abc", Flavor::DotNet), ["abc"]);
        assert_eq!(
            patterns("abcX?", Flavor::DotNet),
            ["abcX", "abcX{1,1}", "abcX{0,0}", "abcX{0,2}", "abcX??"]
        );
        assert_eq!(patterns("abcX??", Flavor::DotNet).len(), 4);
        assert_eq!(
            patterns("abc[XY]", Flavor::DotNet),
            [r"abc[\w\W]", "abc[^XY]", "abc[Y]", "abc[X]"]
        );
        assert_eq!(
            patterns(r"abc\d", Flavor::DotNet),
            ["abcd", r"abc[\d\D]", r"abc\D"]
        );
        assert!(patterns("^abc(d+|[xyz])$", Flavor::DotNet).len() >= 12);
    }

    #[test]
    fn patterns_that_do_not_parse_get_nothing() {
        for pattern in [
            "[Z-A]",
            "\\",
            "(abc",
            r"\p{UnicodeCategory}",
            "a**",
            "*a",
            r"\q",
        ] {
            assert!(patterns(pattern, Flavor::DotNet).is_empty(), "{pattern}");
        }
    }

    #[test]
    fn every_dotnet_mutation() {
        let all = patterns(r"^(?<y>\d{2,4})-(a|b)[a-z]+(?=x)\p{Lu}*?$", Flavor::DotNet);
        for expected in [
            r"(?<y>\d{2,4})-(a|b)[a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>\D{2,4})-(a|b)[a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>d{2,4})-(a|b)[a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>[\d\D]{2,4})-(a|b)[a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d)-(a|b)[a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d{1,4})-(a|b)[a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,5})-(a|b)[a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,4}?)-(a|b)[a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,4})-(?:a|b)[a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,4})-(a|b)[^a-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,4})-(a|b)[b-z]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,4})-(a|b)[a-y]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,4})-(a|b)[\w\W]+(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,4})-(a|b)[a-z]{0,}(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,4})-(a|b)[a-z]{2,}(?=x)\p{Lu}*?$",
            r"^(?<y>\d{2,4})-(a|b)[a-z]+(?!x)\p{Lu}*?$",
            r"^(?<y>\d{2,4})-(a|b)[a-z]+(?=x)\P{Lu}*?$",
            r"^(?<y>\d{2,4})-(a|b)[a-z]+(?=x)\p{Lu}$",
            r"^(?<y>\d{2,4})-(a|b)[a-z]+(?=x)\p{Lu}{1,}?$",
            r"^(?<y>\d{2,4})-(a|b)[a-z]+(?=x)\p{Lu}*?",
        ] {
            assert!(
                all.contains(&expected.to_string()),
                "missing {expected} in {all:#?}"
            );
        }
    }

    #[test]
    fn a_group_with_a_back_reference_stays_capturing() {
        let all = patterns(r"(a)\1", Flavor::DotNet);
        assert!(!all.contains(&r"(?:a)\1".to_string()), "{all:?}");
    }

    #[test]
    fn javascript_gets_level_one_only() {
        assert_eq!(
            patterns(r"^\bab+[xy]\d(?=c)$", JS),
            [
                r"\bab+[xy]\d(?=c)$",
                r"^\bab[xy]\d(?=c)$",
                r"^\bab+[^xy]\d(?=c)$",
                r"^\bab+[xy]\D(?=c)$",
                r"^\bab+[xy]\d(?!c)$",
                r"^\bab+[xy]\d(?=c)",
            ]
        );
        assert_eq!(patterns(r"\p{L}", JS), Vec::<String>::new());
        assert_eq!(
            patterns(r"\p{L}", Flavor::JavaScript { unicode: true }),
            [r"\P{L}"]
        );
    }

    #[test]
    fn ranges_change_within_letters_digits_and_escapes() {
        assert_eq!(
            patterns(r"[0-9]", Flavor::DotNet)
                .into_iter()
                .filter(|p| p.contains('-'))
                .collect::<Vec<_>>(),
            ["[^0-9]", "[1-9]", "[0-8]"]
        );
        assert!(patterns(r"[\x41-\x5a]", Flavor::DotNet).contains(&r"[\x40-\x5a]".to_string()));
    }
}
