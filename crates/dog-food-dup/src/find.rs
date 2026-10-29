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
    /// The copy's code, without its shared indentation, for showing copies
    /// side by side (cut after `CODE_LINES` lines).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub code: String,
    /// A copy whose statements are mixed with others (evidence
    /// `dependence`): the line ranges that are part of it, `[start, end]`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<[usize; 2]>,
}

/// Lines of a copy's code kept in the report.
pub const CODE_LINES: usize = 150;

/// Lines `start..=end` (1-based) of `text`, without the indentation they
/// share, cut after `CODE_LINES`.
pub fn excerpt(text: &str, start: usize, end: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .skip(start.saturating_sub(1))
        .take(end + 1 - start.min(end + 1))
        .collect();
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut kept: Vec<&str> = lines
        .iter()
        .take(CODE_LINES)
        .map(|l| l.get(indent..).unwrap_or(l.trim_start()))
        .collect();
    let more = lines.len().saturating_sub(CODE_LINES);
    let cut = format!("... ({more} more lines)");
    if more > 0 {
        kept.push(&cut);
    }
    kept.join("\n")
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
    /// A name for the class that stays while its copies stay in the same
    /// functions: decisions about it are kept by it (see `decisions`).
    pub id: String,
    /// `copies` of a piece of code, or `repeated`: one statement shape
    /// written several times in a row.
    pub kind: &'static str,
    /// 1: the same code; 2: names, values or types differ; 3: tokens are
    /// also added, removed or changed (see `differences`).
    #[serde(rename = "type")]
    pub clone_type: u8,
    /// How the match was found: `syntax`, the same shape of tokens;
    /// `dependence`, the same statements with the same data flow, in another
    /// order or mixed with others; `normalized` and `query` (the C# engine).
    pub evidence: &'static str,
    /// Type 3: matched tokens over all tokens, 2·common / (a + b), for the
    /// copy least like the first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f64>,
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
    /// Type 3: what each copy adds, removes or changes against the first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub differences: Vec<Difference>,
    /// Type 4: the rules that made the copies' meaning the same.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    /// A repeated query (`kind` `query`): the entity, the table, it reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Type 4 with `--confirm`: what running the copies on the same inputs showed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<Confirmation>,
    /// The riskiest copy: the complexity, coverage and CRAP score of the
    /// function it is in (see `risk`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<crate::risk::Risk>,
    /// A C# copy with `--semantic`: what its first copy takes and gives as
    /// a method, from the compiler's data flow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<crate::semantic::Signature>,
}

/// What running a type-4 class's methods on the same generated inputs showed.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Confirmation {
    /// `confirmed`, or `not-run` (with the reason); a class an input
    /// disproved is not reported.
    pub status: String,
    #[serde(default)]
    pub tries: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One place where a near-miss copy differs from the first copy.
#[derive(Debug, Clone, Serialize)]
pub struct Difference {
    /// The copy, by its position in `copies` (the first is 0).
    pub copy: usize,
    /// `added`, `removed` or `changed`, as the copy has it.
    pub kind: &'static str,
    /// The line in the copy.
    pub line: usize,
    /// What the first copy has there (empty when the copy adds code).
    pub first: String,
    /// What the copy has there (empty when it leaves the code out).
    pub other: String,
}

/// A run of statements of one block, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Span {
    pub block: usize,
    pub first: usize,
    pub last: usize,
}

impl Span {
    fn overlaps(&self, other: &Span) -> bool {
        self.block == other.block && self.first <= other.last && other.first <= self.last
    }

    fn contains(&self, other: &Span) -> bool {
        self.block == other.block && self.first <= other.first && other.last <= self.last
    }
}

pub fn clone_classes(
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
    settings: Settings,
) -> Vec<CloneClass> {
    let shape = |s: &Span, k: usize| blocks[s.block].statements[k].shape;
    let runs = repeated_runs(blocks, settings);
    let mut work = groups(seeds(blocks, settings, &runs), blocks, sources);
    // A group whose copies all match one statement earlier is the tail of
    // an earlier group, which finds them.
    work.retain(|group| !all_match_before(group, &shape));
    let mut classes = grow(work, blocks);
    let lines = classes_lines(&classes, blocks);
    classes.retain(|spans| !within_other_copies(spans, &lines, blocks));
    let mut found = outermost(classes, blocks, sources, display);
    found.extend(
        runs.iter()
            .map(|r| repeated(r, blocks, sources, display))
            .filter(|c| c.saves > 0),
    );
    found.retain(|c| !mostly_parameters(c));
    crate::near::add(&mut found, blocks, sources, display, settings);
    crate::reorder::add(&mut found, blocks, sources, display, settings);
    drop_wrappers(&mut found);
    drop_nested(&mut found);
    found.sort_by(|a, b| {
        b.saves
            .cmp(&a.saves)
            .then(a.copies[0].file.cmp(&b.copies[0].file))
    });
    found
}

/// Seeds: from each statement, the shortest run that is big enough, by the
/// hash of its shapes. Runs inside a repeated run are left to it.
fn seeds(blocks: &[Block], settings: Settings, runs: &[Span]) -> HashMap<u64, Vec<Span>> {
    let mut seeds: HashMap<u64, Vec<Span>> = HashMap::new();
    for (b, block) in blocks.iter().enumerate() {
        for first in 0..block.statements.len() {
            let Some(last) = seed_end(&block.statements, first, settings) else {
                continue;
            };
            let span = Span {
                block: b,
                first,
                last,
            };
            if !runs.iter().any(|r| r.contains(&span)) {
                seeds
                    .entry(shapes_hash(block, first, last))
                    .or_default()
                    .push(span);
            }
        }
    }
    seeds
}

/// The last statement of the shortest run from `first` with enough tokens and lines.
fn seed_end(
    statements: &[crate::tokens::Statement],
    first: usize,
    settings: Settings,
) -> Option<usize> {
    let mut tokens = 0;
    (first..statements.len()).find(|&last| {
        tokens += statements[last].tokens.len();
        let lines = statements[last].end_line + 1 - statements[first].start_line;
        tokens >= settings.min_tokens && lines >= settings.min_lines
    })
}

fn shapes_hash(block: &Block, first: usize, last: usize) -> u64 {
    let bytes: Vec<[u8; 8]> = (first..=last)
        .map(|k| block.statements[k].shape.to_le_bytes())
        .collect();
    let parts: Vec<&[u8]> = bytes.iter().map(|b| b.as_slice()).collect();
    fnv(&parts)
}

/// Seeds of one shape, split by language, without copies that overlap.
fn groups(seeds: HashMap<u64, Vec<Span>>, blocks: &[Block], sources: &[Source]) -> Vec<Vec<Span>> {
    let mut work = Vec::new();
    for group in seeds.into_values() {
        let mut by_language: BTreeMap<Option<String>, Vec<Span>> = BTreeMap::new();
        for span in group {
            by_language
                .entry(extension(blocks, sources, &span))
                .or_default()
                .push(span);
        }
        work.extend(by_language.into_values().map(apart).filter(|g| g.len() > 1));
    }
    work
}

/// Each group grows together, splitting where its members' next
/// statements differ: every copy grows as far as another stays alike,
/// without comparing pairs, so a pattern copied hundreds of times costs no
/// more than its copies. A group is recorded where it splits or ends.
fn grow(mut work: Vec<Vec<Span>>, blocks: &[Block]) -> Vec<Vec<Span>> {
    let mut classes = Vec::new();
    while let Some(group) = work.pop() {
        let (grown, ended) = next_statements(&group, blocks);
        let growing: usize = grown.iter().map(Vec::len).sum();
        if ended || growing < group.len() || grown.len() > 1 {
            classes.push(group);
        }
        work.extend(grown);
    }
    classes
}

/// The group's members one statement longer, by the shape of that
/// statement; and whether a member reached the end of its block.
fn next_statements(group: &[Span], blocks: &[Block]) -> (Vec<Vec<Span>>, bool) {
    let mut next: HashMap<u64, Vec<Span>> = HashMap::new();
    let mut ended = false;
    for s in group {
        match blocks[s.block].statements.get(s.last + 1) {
            Some(statement) => next.entry(statement.shape).or_default().push(Span {
                last: s.last + 1,
                ..*s
            }),
            None => ended = true,
        }
    }
    let grown = next
        .into_values()
        .map(apart)
        .filter(|g| g.len() > 1)
        .collect();
    (grown, ended)
}

/// The classes with each copy that is inside a bigger copy left out (it is
/// reported with that one), described.
fn outermost(
    classes: Vec<Vec<Span>>,
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
) -> Vec<CloneClass> {
    let mut by_block: HashMap<usize, Vec<Span>> = HashMap::new();
    for span in classes.iter().flatten() {
        by_block.entry(span.block).or_default().push(*span);
    }
    let inside = |s: &Span| by_block[&s.block].iter().any(|o| o != s && o.contains(s));
    classes
        .into_iter()
        .filter_map(|spans| {
            let mut kept: Vec<Span> = Vec::new();
            for s in spans.into_iter().filter(|s| !inside(s)) {
                if !kept.iter().any(|k| k.overlaps(&s)) {
                    kept.push(s);
                }
            }
            (kept.len() > 1).then(|| describe(&kept, blocks, sources, display))
        })
        .filter(|c| c.saves > 0)
        .collect()
}

/// Every class's copies as (file, first line, last line, from an expression block).
fn classes_lines(classes: &[Vec<Span>], blocks: &[Block]) -> Vec<Vec<(usize, usize, usize, bool)>> {
    classes
        .iter()
        .map(|spans| {
            spans
                .iter()
                .map(|s| {
                    let block = &blocks[s.block];
                    (
                        block.file,
                        block.statements[s.first].start_line,
                        block.statements[s.last].end_line,
                        block.is_expression,
                    )
                })
                .collect()
        })
        .collect()
}

/// A class of expression copies each inside a copy of statements: that
/// class says it already, with the statements around them.
fn within_other_copies(
    spans: &[Span],
    all: &[Vec<(usize, usize, usize, bool)>],
    blocks: &[Block],
) -> bool {
    spans.iter().all(|s| {
        let block = &blocks[s.block];
        let (start, end) = (
            block.statements[s.first].start_line,
            block.statements[s.last].end_line,
        );
        block.is_expression
            && all.iter().flatten().any(|&(file, a, b, expression)| {
                !expression && file == block.file && a <= start && end <= b
            })
    })
}

/// Copies that share a shape but little else: more than two parameters per
/// statement (mapping code, guard clauses on other fields) would make a
/// shared version that is mostly its arguments. Repeated runs are kept, as
/// a table of what differs takes those.
pub(crate) fn mostly_parameters(class: &CloneClass) -> bool {
    let parameters = class
        .holes
        .iter()
        .filter(|h| h.kind != HoleKind::Local)
        .count();
    class.kind != "repeated" && parameters > 2 * class.statements.max(1)
}

/// Fewest statements in a row with one shape that make a repeated run.
const MIN_REPEATS: usize = 3;

/// Runs of statements in a row that share one shape (`if (dto.X != null)
/// user.X = dto.X;` for each field), big enough together.
fn repeated_runs(blocks: &[Block], settings: Settings) -> Vec<Span> {
    let mut runs = Vec::new();
    for (b, block) in blocks.iter().enumerate() {
        let mut first = 0;
        while first < block.statements.len() {
            let last = same_shape_end(&block.statements, first);
            let span = Span {
                block: b,
                first,
                last,
            };
            if is_run(block, &span, settings) {
                runs.push(span);
            }
            first = last + 1;
        }
    }
    runs
}

/// The last statement from `first` on with the shape of `first`.
fn same_shape_end(statements: &[crate::tokens::Statement], first: usize) -> usize {
    let shape = statements[first].shape;
    first
        + statements[first + 1..]
            .iter()
            .take_while(|s| s.shape == shape)
            .count()
}

fn is_run(block: &Block, span: &Span, settings: Settings) -> bool {
    let tokens: usize = block.statements[span.first..=span.last]
        .iter()
        .map(|s| s.tokens.len())
        .sum();
    span.last + 1 - span.first >= MIN_REPEATS && tokens >= settings.min_tokens
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
            // Near-miss bodies are united whole, whatever they hold.
            classes[n].clone_type != 3
                && classes.iter().enumerate().any(|(m, other)| {
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

/// A class whose every copy lies inside a copy of another class, with as
/// many copies: the same functions found again for a block nested in the
/// bigger copy, which says it already.
fn drop_nested(classes: &mut Vec<CloneClass>) {
    let inside = |inner: &Copy, outer: &Copy| {
        inner.file == outer.file
            && outer.start_line <= inner.start_line
            && inner.end_line <= outer.end_line
            && (outer.start_line, outer.end_line) != (inner.start_line, inner.end_line)
    };
    let nested: BTreeSet<usize> = (0..classes.len())
        .filter(|&n| {
            classes.iter().enumerate().any(|(m, other)| {
                m != n
                    && other.kind == classes[n].kind
                    && other.copies.len() >= classes[n].copies.len()
                    && classes[n]
                        .copies
                        .iter()
                        .all(|inner| other.copies.iter().any(|outer| inside(inner, outer)))
            })
        })
        .collect();
    let mut n = 0;
    classes.retain(|_| {
        n += 1;
        !nested.contains(&(n - 1))
    });
}

/// The file extension of a span: copies in different languages cannot
/// share code.
fn extension(blocks: &[Block], sources: &[Source], span: &Span) -> Option<String> {
    sources[blocks[span.block].file]
        .path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// The members of a group that do not overlap one taken before them (a
/// pattern that repeats inside one block matches itself shifted).
fn apart(mut group: Vec<Span>) -> Vec<Span> {
    group.sort();
    let mut kept: Vec<Span> = Vec::new();
    for span in group {
        if !kept.iter().any(|k| k.overlaps(&span)) {
            kept.push(span);
        }
    }
    kept
}

/// Whether every member of `group` is preceded by a statement of one shape,
/// and the group grown by it would still not overlap.
fn all_match_before(group: &[Span], shape: &dyn Fn(&Span, usize) -> u64) -> bool {
    let before = |s: &Span| (s.first > 0).then(|| shape(s, s.first - 1));
    let Some(first) = before(&group[0]) else {
        return false;
    };
    let earlier: Vec<Span> = group
        .iter()
        .map(|s| Span {
            first: s.first.saturating_sub(1),
            ..*s
        })
        .collect();
    group.iter().all(|s| before(s) == Some(first))
        && earlier
            .iter()
            .enumerate()
            .all(|(n, a)| earlier[n + 1..].iter().all(|b| !a.overlaps(b)))
}

fn tokens<'b>(blocks: &'b [Block], span: &Span) -> Vec<&'b Token> {
    blocks[span.block].statements[span.first..=span.last]
        .iter()
        .flat_map(|s| &s.tokens)
        .collect()
}

pub(crate) fn describe(
    spans: &[Span],
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
) -> CloneClass {
    let copies: Vec<Copy> = spans
        .iter()
        .map(|s| copy_of(s, blocks, sources, display))
        .collect();
    let streams: Vec<Vec<&Token>> = spans.iter().map(|s| tokens(blocks, s)).collect();
    let first = &streams[0];
    let declared = declarations(first);
    let (holes, at) = anti_unify(&streams, &declared);
    let span = spans[0];
    let lines = copies[0].end_line + 1 - copies[0].start_line;
    let parameters = holes.iter().filter(|h| h.kind != HoleKind::Local).count();
    let saves =
        ((copies.len() - 1) * lines) as i64 - copies.len() as i64 - 2 - (parameters / 3) as i64;
    // Whole bodies are kept as one function: its returns stay as they are.
    let cautions = if whole_bodies(spans, blocks) {
        Vec::new()
    } else {
        cautions(first, &declared, blocks, &span)
    };
    CloneClass {
        id: String::new(),
        kind: "copies",
        clone_type: if holes.is_empty() { 1 } else { 2 },
        evidence: "syntax",
        similarity: None,
        differences: Vec::new(),
        rules: Vec::new(),
        entity: None,
        confirmation: None,
        risk: None,
        signature: None,
        unite: unite(&copies, &holes, spans, blocks),
        copies,
        lines,
        tokens: first.len(),
        statements: span.last + 1 - span.first,
        template: template(&span, first, &at, blocks, sources),
        holes,
        saves,
        cautions,
    }
}

fn copy_of(
    span: &Span,
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
) -> Copy {
    let block = &blocks[span.block];
    Copy {
        file: display(&sources[block.file].path),
        start_line: block.statements[span.first].start_line,
        end_line: block.statements[span.last].end_line,
        function: block.function.clone(),
        changed: false,
        code: excerpt(
            &sources[block.file].text,
            block.statements[span.first].start_line,
            block.statements[span.last].end_line,
        ),
        parts: Vec::new(),
    }
}

/// Anti-unification: a hole wherever the copies differ, the same
/// differences everywhere being one hole; and where each hole is, as
/// (token position, hole number).
fn anti_unify(
    streams: &[Vec<&Token>],
    declared: &BTreeSet<usize>,
) -> (Vec<Hole>, Vec<(usize, usize)>) {
    let first = &streams[0];
    let mut holes: Vec<Hole> = Vec::new();
    let mut by_values: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    let mut at = Vec::new();
    for position in 0..first.len() {
        let values: Vec<String> = streams.iter().map(|s| s[position].text.clone()).collect();
        if first[position].kind == Kind::Other || values.iter().all(|v| *v == values[0]) {
            continue;
        }
        let index = *by_values.entry(values.clone()).or_insert_with(|| {
            holes.push(Hole {
                index: holes.len() + 1,
                kind: hole_kind(first, position),
                values,
            });
            holes.len()
        });
        if declared.contains(&position) {
            holes[index - 1].kind = HoleKind::Local;
        }
        at.push((position, index));
    }
    (holes, at)
}

/// The first copy's text with `⟨n⟩` at each hole, without its shared indentation.
fn template(
    span: &Span,
    first: &[&Token],
    at: &[(usize, usize)],
    blocks: &[Block],
    sources: &[Source],
) -> String {
    let block = &blocks[span.block];
    let text = &sources[block.file].text;
    let start = block.statements[span.first].start_byte;
    let end = block.statements[span.last].end_byte;
    let mut template = String::new();
    let mut cursor = start;
    for (position, index) in at {
        let token = first[*position];
        template.push_str(&text[cursor..token.start_byte]);
        template.push_str(&format!("⟨{index}⟩"));
        cursor = token.end_byte;
    }
    template.push_str(&text[cursor..end]);
    dedent(
        &template,
        start - text[..start].rfind('\n').map_or(0, |n| n + 1),
    )
}

/// Every copy is a function's whole body.
fn whole_bodies(spans: &[Span], blocks: &[Block]) -> bool {
    spans.iter().all(|s| {
        blocks[s.block].is_body && s.first == 0 && s.last + 1 == blocks[s.block].statements.len()
    })
}

/// Positions of names the copy declares: `var x`, `let x`, `int x =`,
/// `foreach (var x in`, lambda parameters `x =>`.
pub(crate) fn declarations(tokens: &[&Token]) -> BTreeSet<usize> {
    let names: BTreeSet<&str> = (0..tokens.len())
        .filter(|&n| declares(tokens, n))
        .map(|n| tokens[n].text.as_str())
        .collect();
    // Every later use of a declared name is the same local.
    tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == Kind::Name && names.contains(t.text.as_str()))
        .map(|(n, _)| n)
        .collect()
}

/// Whether the name at `n` is declared there.
fn declares(tokens: &[&Token], n: usize) -> bool {
    if tokens[n].kind != Kind::Name {
        return false;
    }
    let before = n.checked_sub(1).map(|p| tokens[p]);
    let after = tokens.get(n + 1).map(|t| t.text.as_str());
    let keyword =
        before.is_some_and(|b| matches!(b.text.as_str(), "var" | "let" | "const" | "mut"));
    keyword || after == Some("=>") || before.is_some_and(|b| typed(b, after))
}

/// `Type name =`, `int name;`, `Type name in`: a name after a type.
fn typed(before: &Token, after: Option<&str>) -> bool {
    let a_type = before.kind == Kind::Name || PRIMITIVES.contains(&before.text.as_str());
    a_type && matches!(after, Some("=" | "in" | ";"))
}

const PRIMITIVES: &[&str] = &[
    "int", "long", "short", "byte", "bool", "string", "decimal", "double", "float", "char",
    "object", "var", "usize", "isize", "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f32",
    "f64", "number", "boolean",
];

pub(crate) fn hole_kind(tokens: &[&Token], position: usize) -> HoleKind {
    if tokens[position].kind != Kind::Name {
        return HoleKind::Value;
    }
    let before = position.checked_sub(1).map(|p| tokens[p].text.as_str());
    let after = tokens.get(position + 1);
    if after.is_some_and(|a| a.text == "(") {
        return HoleKind::Call;
    }
    if matches!(before, Some("." | "?." | "->" | "::")) {
        return HoleKind::Member;
    }
    if is_type(&tokens[position].text, before, after) {
        return HoleKind::Type;
    }
    HoleKind::Name
}

/// A capitalized name where a type goes: after `new`, `:`, `<`, `as`,
/// `is`, or before `>` or a name.
fn is_type(text: &str, before: Option<&str>, after: Option<&&Token>) -> bool {
    let capital = text.chars().next().is_some_and(char::is_uppercase);
    let after_type = after.is_some_and(|a| a.text == ">" || a.kind == Kind::Name);
    capital && (matches!(before, Some("new" | ":" | "<" | "as" | "is")) || after_type)
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
    let place = place(copies, whole_bodies(spans, blocks));
    if parameters.is_empty() {
        format!(
            "{place}; the copies are identical apart from local names, so it needs no parameters for what differs, only the values it reads from around it"
        )
    } else {
        format!(
            "{place}, taking {} ({})",
            holes_text(holes),
            parameters.join("; ")
        )
    }
}

/// Where a shared version goes: one function kept, a private method in the
/// type, a function in the file, or a helper the files share.
fn place(copies: &[Copy], whole: bool) -> String {
    let owner = |c: &Copy| {
        c.function
            .as_deref()
            .and_then(|f| f.rsplit_once('.'))
            .map(|(owner, _)| owner.to_string())
    };
    let files: BTreeSet<&str> = copies.iter().map(|c| c.file.as_str()).collect();
    let owners: BTreeSet<Option<String>> = copies.iter().map(owner).collect();
    if whole && copies.iter().all(|c| c.function.is_some()) {
        return "the functions' bodies are copies: keep one function and call it from the others"
            .to_string();
    }
    match (files.len(), owners.len()) {
        (1, 1) => match owners.into_iter().next().flatten() {
            Some(owner) => format!("extract a private method in {owner} and call it from each copy"),
            None => "extract a function in this file and call it from each copy".to_string(),
        },
        (1, _) => "extract a shared function in this file (a static helper the types call)".to_string(),
        (n, _) => format!(
            "move it into one shared helper that the {n} files call (a static method, an extension or a base type)"
        ),
    }
}

/// The template without the indentation its lines share.
/// The first line starts at `column` of its line in the file: the lines
/// after it lose that much indentation, keeping their own on top.
pub(crate) fn dedent(text: &str, column: usize) -> String {
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
