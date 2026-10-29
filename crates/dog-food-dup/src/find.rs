//! Finding the copies: runs of statements with the same shape, extended as
//! far as they stay alike and grouped into clone classes; then, for each
//! class, the template its copies share and what uniting them takes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::tokens::{fnv, Block, Kind, Source, Token};

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Fewest tokens a copy has: below that, sharing code costs more than it saves.
    pub min_tokens: usize,
    /// Fewest lines a copy spans.
    pub min_lines: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            min_tokens: 50,
            min_lines: 5,
        }
    }
}

/// One copy: where it is and the function it is in.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Copy {
    pub file: String,
    pub start_line: usize,
    pub end_line: usize,
    pub function: Option<String>,
    /// With `--changes`: the copy overlaps lines the branch changed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub changed: bool,
}

/// What differs at a hole of the template, which decides how a shared
/// version takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HoleKind {
    /// A literal: a plain parameter.
    Value,
    /// A variable or parameter from outside the copy: a parameter.
    Name,
    /// A member read off an object: pass the value, or a selector.
    Member,
    /// A function or method called: pass a function.
    Call,
    /// A type: a generic parameter.
    Type,
    /// A local the copy declares itself: only renamed, no parameter needed.
    Local,
}

impl HoleKind {
    /// One word for the kind.
    pub fn name(self) -> &'static str {
        match self {
            HoleKind::Value => "value",
            HoleKind::Name => "name",
            HoleKind::Member => "member",
            HoleKind::Call => "call",
            HoleKind::Type => "type",
            HoleKind::Local => "local",
        }
    }

    /// The kind, for several holes.
    pub fn plural(self) -> &'static str {
        match self {
            HoleKind::Value => "values",
            HoleKind::Name => "variables",
            HoleKind::Member => "members",
            HoleKind::Call => "calls",
            HoleKind::Type => "types",
            HoleKind::Local => "locals",
        }
    }

    /// How a shared version takes holes of this kind.
    pub fn advice(self) -> &'static str {
        match self {
            HoleKind::Value | HoleKind::Name => "values and variables become parameters",
            HoleKind::Member => "for members pass the values, or a selector such as `x => x.Price`",
            HoleKind::Call => "for calls pass the function, a delegate or a lambda",
            HoleKind::Type => "types become generic type parameters",
            HoleKind::Local => "locals are only renamed",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            HoleKind::Value => "a value: a parameter",
            HoleKind::Name => "a variable from outside: a parameter",
            HoleKind::Member => "a member: pass the value, or a selector such as `x => x.Price`",
            HoleKind::Call => "a call: pass the function (a delegate or lambda)",
            HoleKind::Type => "a type: a generic type parameter",
            HoleKind::Local => "a local the copy declares: only renamed, not a parameter",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Hole {
    /// Its number in the template, `⟨n⟩`.
    pub index: usize,
    pub kind: HoleKind,
    /// What each copy has there, in the order of `copies`.
    pub values: Vec<String>,
}

/// Copies of one piece of code, and how to unite them.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloneClass {
    /// `copies` of a piece of code, or `repeated`: one statement shape
    /// written several times in a row.
    pub kind: &'static str,
    pub copies: Vec<Copy>,
    /// Lines and tokens of one copy.
    pub lines: usize,
    pub tokens: usize,
    pub statements: usize,
    /// The first copy with `⟨n⟩` where the copies differ.
    pub template: String,
    pub holes: Vec<Hole>,
    /// Lines a shared version would save, after its signature and calls.
    pub saves: i64,
    /// What to do: extract a method, a shared helper, keep one function.
    pub unite: String,
    /// What makes uniting harder: values the code after a copy uses, jumps.
    pub cautions: Vec<String>,
}

/// A run of statements of one block, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Span {
    block: usize,
    first: usize,
    last: usize,
}

impl Span {
    fn overlaps(&self, other: &Span) -> bool {
        self.block == other.block && self.first <= other.last && other.first <= self.last
    }

    fn contains(&self, other: &Span) -> bool {
        self.block == other.block && self.first <= other.first && other.last <= self.last
    }
}

/// Most copies of one seed compared pairwise: a pattern repeated hundreds of
/// times (generated code, a test table) would otherwise take quadratic time.
const MAX_SEEDS: usize = 64;

pub fn clone_classes(
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
    settings: Settings,
) -> Vec<CloneClass> {
    let shape = |s: &Span, k: usize| blocks[s.block].statements[k].shape;
    let runs = repeated_runs(blocks, settings);
    let in_run = |b: usize, first: usize, last: usize| {
        runs.iter()
            .any(|r| r.block == b && r.first <= first && last <= r.last)
    };
    // Seeds: the shortest run from each statement that is big enough.
    let mut seeds: HashMap<u64, Vec<Span>> = HashMap::new();
    for (b, block) in blocks.iter().enumerate() {
        let statements = &block.statements;
        for first in 0..statements.len() {
            let mut tokens = 0;
            for last in first..statements.len() {
                tokens += statements[last].tokens.len();
                let lines = statements[last].end_line + 1 - statements[first].start_line;
                if tokens >= settings.min_tokens && lines >= settings.min_lines {
                    if in_run(b, first, last) {
                        break;
                    }
                    let bytes: Vec<[u8; 8]> = (first..=last)
                        .map(|k| statements[k].shape.to_le_bytes())
                        .collect();
                    let parts: Vec<&[u8]> = bytes.iter().map(|b| b.as_slice()).collect();
                    seeds.entry(fnv(&parts)).or_default().push(Span {
                        block: b,
                        first,
                        last,
                    });
                    break;
                }
            }
        }
    }
    // Pairs of seeds, each grown as far as both stay alike.
    let mut classes: HashMap<u64, BTreeSet<Span>> = HashMap::new();
    for group in seeds.values().filter(|g| g.len() > 1) {
        let group = &group[..group.len().min(MAX_SEEDS)];
        for (n, a) in group.iter().enumerate() {
            for b in &group[n + 1..] {
                if a.overlaps(b) || same_language_mismatch(blocks, sources, a, b) {
                    continue;
                }
                // A pair that also matches one statement earlier belongs to an earlier seed.
                if a.first > 0 && b.first > 0 && shape(a, a.first - 1) == shape(b, b.first - 1) {
                    let earlier_a = Span {
                        first: a.first - 1,
                        ..*a
                    };
                    let earlier_b = Span {
                        first: b.first - 1,
                        ..*b
                    };
                    if !earlier_a.overlaps(&earlier_b) {
                        continue;
                    }
                }
                let (mut ra, mut rb) = (*a, *b);
                while ra.last + 1 < blocks[ra.block].statements.len()
                    && rb.last + 1 < blocks[rb.block].statements.len()
                    && shape(&ra, ra.last + 1) == shape(&rb, rb.last + 1)
                {
                    let (na, nb) = (
                        Span {
                            last: ra.last + 1,
                            ..ra
                        },
                        Span {
                            last: rb.last + 1,
                            ..rb
                        },
                    );
                    if na.overlaps(&nb) {
                        break;
                    }
                    ra = na;
                    rb = nb;
                }
                let bytes: Vec<[u8; 8]> = (ra.first..=ra.last)
                    .map(|k| shape(&ra, k).to_le_bytes())
                    .collect();
                let parts: Vec<&[u8]> = bytes.iter().map(|b| b.as_slice()).collect();
                let entry = classes.entry(fnv(&parts)).or_default();
                entry.insert(ra);
                entry.insert(rb);
            }
        }
    }
    // A copy inside a bigger copy is already reported with it.
    let all: Vec<Span> = classes.values().flatten().copied().collect();
    let mut found: Vec<CloneClass> = classes
        .into_values()
        .filter_map(|spans| {
            let mut kept: Vec<Span> = Vec::new();
            for s in spans {
                let inside = all.iter().any(|o| *o != s && o.contains(&s));
                if !inside && !kept.iter().any(|k| k.overlaps(&s)) {
                    kept.push(s);
                }
            }
            (kept.len() > 1).then(|| describe(&kept, blocks, sources, display))
        })
        .filter(|c| c.saves > 0)
        .collect();
    found.extend(
        runs.iter()
            .map(|r| repeated(r, blocks, sources, display))
            .filter(|c| c.saves > 0),
    );
    drop_wrappers(&mut found);
    found.sort_by(|a, b| {
        b.saves
            .cmp(&a.saves)
            .then(a.copies[0].file.cmp(&b.copies[0].file))
    });
    found
}

/// Fewest statements in a row with one shape that make a repeated run.
const MIN_REPEATS: usize = 3;

/// Runs of statements in a row that share one shape (`if (dto.X != null)
/// user.X = dto.X;` for each field), big enough together.
fn repeated_runs(blocks: &[Block], settings: Settings) -> Vec<Span> {
    let mut runs = Vec::new();
    for (b, block) in blocks.iter().enumerate() {
        let statements = &block.statements;
        let mut first = 0;
        while first < statements.len() {
            let mut last = first;
            while last + 1 < statements.len()
                && statements[last + 1].shape == statements[first].shape
            {
                last += 1;
            }
            let tokens: usize = statements[first..=last]
                .iter()
                .map(|s| s.tokens.len())
                .sum();
            if last + 1 - first >= MIN_REPEATS && tokens >= settings.min_tokens {
                runs.push(Span {
                    block: b,
                    first,
                    last,
                });
            }
            first = last + 1;
        }
    }
    runs
}

/// A repeated run as a class: each statement is a copy of the template.
fn repeated(
    run: &Span,
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
) -> CloneClass {
    let spans: Vec<Span> = (run.first..=run.last)
        .map(|k| Span {
            block: run.block,
            first: k,
            last: k,
        })
        .collect();
    let mut class = describe(&spans, blocks, sources, display);
    let block = &blocks[run.block];
    let lines = block.statements[run.last].end_line + 1 - block.statements[run.first].start_line;
    let count = spans.len();
    class.kind = "repeated";
    // A loop over a table of the differing parts replaces the copies.
    class.saves = lines as i64 - (class.lines + count + 2) as i64;
    class.unite = format!(
        "{count} statements in a row share this shape ({lines} lines): put what differs ({}) in a list and loop over it, or call one helper {count} times",
        holes_text(&class.holes)
    );
    class
}

/// The holes, grouped by kind: `⟨1⟩, ⟨2⟩ members`.
fn holes_text(holes: &[Hole]) -> String {
    let mut by_kind: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for hole in holes.iter().filter(|h| h.kind != HoleKind::Local) {
        by_kind
            .entry(hole.kind.plural())
            .or_default()
            .push(format!("⟨{}⟩", hole.index));
    }
    if by_kind.is_empty() {
        return "nothing but local names".to_string();
    }
    by_kind
        .into_iter()
        .map(|(kind, holes)| format!("{} {kind}", holes.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

/// A class whose every copy only wraps a copy of a class with more copies
/// (a loop around a repeated body): uniting the inner one is the better
/// refactor, and it covers these too.
fn drop_wrappers(classes: &mut Vec<CloneClass>) {
    let inside = |outer: &Copy, inner: &Copy| {
        outer.file == inner.file
            && outer.start_line <= inner.start_line
            && inner.end_line <= outer.end_line
    };
    let wrappers: BTreeSet<usize> = (0..classes.len())
        .filter(|&n| {
            classes.iter().enumerate().any(|(m, other)| {
                m != n
                    && other.copies.len() > classes[n].copies.len()
                    && classes[n]
                        .copies
                        .iter()
                        .all(|outer| other.copies.iter().any(|inner| inside(outer, inner)))
            })
        })
        .collect();
    let mut n = 0;
    classes.retain(|_| {
        n += 1;
        !wrappers.contains(&(n - 1))
    });
}

/// Copies in different languages cannot share code.
fn same_language_mismatch(blocks: &[Block], sources: &[Source], a: &Span, b: &Span) -> bool {
    let extension = |s: &Span| {
        sources[blocks[s.block].file]
            .path
            .extension()
            .map(|e| e.to_ascii_lowercase())
    };
    extension(a) != extension(b)
}

fn tokens<'b>(blocks: &'b [Block], span: &Span) -> Vec<&'b Token> {
    blocks[span.block].statements[span.first..=span.last]
        .iter()
        .flat_map(|s| &s.tokens)
        .collect()
}

fn describe(
    spans: &[Span],
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
) -> CloneClass {
    let copies: Vec<Copy> = spans
        .iter()
        .map(|s| {
            let block = &blocks[s.block];
            Copy {
                file: display(&sources[block.file].path),
                start_line: block.statements[s.first].start_line,
                end_line: block.statements[s.last].end_line,
                function: block.function.clone(),
                changed: false,
            }
        })
        .collect();
    let streams: Vec<Vec<&Token>> = spans.iter().map(|s| tokens(blocks, s)).collect();
    let first = &streams[0];
    let declared = declarations(first);
    // Anti-unification: a hole wherever the copies differ; the same
    // differences everywhere are one hole.
    let mut holes: Vec<Hole> = Vec::new();
    let mut by_values: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    let mut at: Vec<(usize, usize)> = Vec::new();
    for position in 0..first.len() {
        let values: Vec<String> = streams.iter().map(|s| s[position].text.clone()).collect();
        if first[position].kind == Kind::Other || values.iter().all(|v| *v == values[0]) {
            continue;
        }
        let index = *by_values.entry(values.clone()).or_insert_with(|| {
            holes.push(Hole {
                index: holes.len() + 1,
                kind: hole_kind(first, position),
                values: values.clone(),
            });
            holes.len()
        });
        if declared.contains(&position) {
            holes[index - 1].kind = HoleKind::Local;
        }
        at.push((position, index));
    }
    let span = spans[0];
    let block = &blocks[span.block];
    let text = &sources[block.file].text;
    let start = block.statements[span.first].start_byte;
    let end = block.statements[span.last].end_byte;
    let mut template = String::new();
    let mut cursor = start;
    for (position, index) in &at {
        let token = first[*position];
        template.push_str(&text[cursor..token.start_byte]);
        template.push_str(&format!("⟨{index}⟩"));
        cursor = token.end_byte;
    }
    template.push_str(&text[cursor..end]);
    let lines = copies[0].end_line + 1 - copies[0].start_line;
    let parameters = holes.iter().filter(|h| h.kind != HoleKind::Local).count();
    let saves =
        ((copies.len() - 1) * lines) as i64 - copies.len() as i64 - 2 - (parameters / 3) as i64;
    let mut cautions = cautions(first, &declared, blocks, &span);
    // Whole bodies are kept as one function: its returns stay as they are.
    let whole_bodies = spans.iter().all(|s| {
        blocks[s.block].is_body && s.first == 0 && s.last + 1 == blocks[s.block].statements.len()
    });
    if whole_bodies {
        cautions.clear();
    }
    CloneClass {
        kind: "copies",
        unite: unite(&copies, &holes, spans, blocks),
        copies,
        lines,
        tokens: first.len(),
        statements: span.last + 1 - span.first,
        template: dedent(
            &template,
            start - text[..start].rfind('\n').map_or(0, |n| n + 1),
        ),
        holes,
        saves,
        cautions,
    }
}

/// Positions of names the copy declares: `var x`, `let x`, `int x =`,
/// `foreach (var x in`, lambda parameters `x =>`.
fn declarations(tokens: &[&Token]) -> BTreeSet<usize> {
    let mut found = BTreeSet::new();
    for (n, token) in tokens.iter().enumerate() {
        if token.kind != Kind::Name {
            continue;
        }
        let before = n.checked_sub(1).map(|p| tokens[p].text.as_str());
        let after = tokens.get(n + 1).map(|t| t.text.as_str());
        let keyword = matches!(before, Some("var" | "let" | "const" | "mut"));
        let typed = n > 0
            && (tokens[n - 1].kind == Kind::Name
                || PRIMITIVES.contains(&tokens[n - 1].text.as_str()))
            && matches!(after, Some("=" | "in" | ";"));
        let lambda = after == Some("=>");
        if keyword || typed || lambda {
            found.insert(n);
        }
    }
    // Every later use of a declared name is the same local.
    let names: BTreeSet<&str> = found.iter().map(|n| tokens[*n].text.as_str()).collect();
    tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == Kind::Name && names.contains(t.text.as_str()))
        .map(|(n, _)| n)
        .collect()
}

const PRIMITIVES: &[&str] = &[
    "int", "long", "short", "byte", "bool", "string", "decimal", "double", "float", "char",
    "object", "var", "usize", "isize", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32",
    "f64", "number", "boolean",
];

fn hole_kind(tokens: &[&Token], position: usize) -> HoleKind {
    let token = tokens[position];
    match token.kind {
        Kind::Text | Kind::Number => return HoleKind::Value,
        Kind::Other => return HoleKind::Value,
        Kind::Name => {}
    }
    let before = position.checked_sub(1).map(|p| tokens[p].text.as_str());
    let after = tokens.get(position + 1).map(|t| t.text.as_str());
    let member = matches!(before, Some("." | "?." | "->" | "::"));
    if after == Some("(") {
        return HoleKind::Call;
    }
    if member {
        return HoleKind::Member;
    }
    let capital = token.text.chars().next().is_some_and(char::is_uppercase);
    let typed_position = matches!(before, Some("new" | ":" | "<" | "as" | "is"))
        || after.is_some_and(|a| {
            a == ">"
                || tokens
                    .get(position + 1)
                    .is_some_and(|t| t.kind == Kind::Name)
        });
    if capital && typed_position {
        return HoleKind::Type;
    }
    HoleKind::Name
}

/// What makes extracting the copy harder.
fn cautions(
    tokens: &[&Token],
    declared: &BTreeSet<usize>,
    blocks: &[Block],
    span: &Span,
) -> Vec<String> {
    let mut found = Vec::new();
    let jumps: BTreeSet<&str> = tokens
        .iter()
        .map(|t| t.text.as_str())
        .filter(|t| matches!(*t, "return" | "break" | "continue" | "yield" | "goto"))
        .collect();
    if !jumps.is_empty() {
        let list: Vec<&str> = jumps.into_iter().collect();
        found.push(format!(
            "it leaves the enclosing code ({}): a shared version has to tell its caller to do the same",
            list.join(", ")
        ));
    }
    let names: BTreeSet<&str> = declared.iter().map(|n| tokens[*n].text.as_str()).collect();
    let later: BTreeSet<&str> = blocks[span.block].statements[span.last + 1..]
        .iter()
        .flat_map(|s| &s.tokens)
        .filter(|t| t.kind == Kind::Name && names.contains(t.text.as_str()))
        .map(|t| t.text.as_str())
        .collect();
    if !later.is_empty() {
        let list: Vec<&str> = later.into_iter().collect();
        found.push(format!(
            "the code after the first copy uses {}: a shared version returns {}",
            list.join(", "),
            if list.len() == 1 {
                "it"
            } else {
                "them (a tuple or a small type)"
            }
        ));
    }
    found
}

/// How to unite the copies.
fn unite(copies: &[Copy], holes: &[Hole], spans: &[Span], blocks: &[Block]) -> String {
    let kinds: BTreeSet<&'static str> = holes
        .iter()
        .filter(|h| h.kind != HoleKind::Local)
        .map(|h| h.kind.advice())
        .collect();
    let parameters: Vec<&str> = kinds.into_iter().collect();
    let owner = |c: &Copy| {
        c.function
            .as_deref()
            .and_then(|f| f.rsplit_once('.'))
            .map(|(owner, _)| owner.to_string())
    };
    let files: BTreeSet<&str> = copies.iter().map(|c| c.file.as_str()).collect();
    let owners: BTreeSet<Option<String>> = copies.iter().map(owner).collect();
    let whole = spans.iter().all(|s| {
        blocks[s.block].is_body && s.first == 0 && s.last + 1 == blocks[s.block].statements.len()
    });
    let place = if whole && copies.iter().all(|c| c.function.is_some()) {
        "the functions' bodies are copies: keep one function and call it from the others"
            .to_string()
    } else if files.len() == 1 && owners.len() == 1 {
        match owners.into_iter().next().flatten() {
            Some(owner) => {
                format!("extract a private method in {owner} and call it from each copy")
            }
            None => "extract a function in this file and call it from each copy".to_string(),
        }
    } else if files.len() == 1 {
        "extract a shared function in this file (a static helper the types call)".to_string()
    } else {
        format!(
            "move it into one shared helper that the {} files call (a static method, an extension or a base type)",
            files.len()
        )
    };
    if parameters.is_empty() {
        format!(
            "{place}; the copies are identical apart from local names, so it takes no parameters"
        )
    } else {
        format!(
            "{place}, taking {} ({})",
            holes_text(holes),
            parameters.join("; ")
        )
    }
}

/// The template without the indentation its lines share.
/// The first line starts at `column` of its line in the file: the lines
/// after it lose that much indentation, keeping their own on top.
fn dedent(text: &str, column: usize) -> String {
    text.lines()
        .enumerate()
        .map(|(n, l)| {
            if n == 0 {
                return l;
            }
            let own = l.len() - l.trim_start().len();
            &l[own.min(column)..]
        })
        .collect::<Vec<_>>()
        .join("\n")
}
