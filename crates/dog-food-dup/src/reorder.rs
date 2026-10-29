//! Copies whose statements are in another order, or mixed with other code:
//! the in-order passes miss them, as a copy there is a run of statements in
//! the same order.
//!
//! Two blocks' statements are paired by shape, with one consistent renaming,
//! and a pair is kept only where the data flows the same way: every name a
//! paired statement reads comes, in both copies, from paired statements
//! (each other's partners) or from outside the copy. What is left are the
//! same statements computing the same values, in whatever order and with
//! whatever in between (Komondoor and Horwitz's slicing, on tokens).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::find::{excerpt, CloneClass, Copy, Settings};
use crate::tokens::{Block, Kind, Source, Statement};

/// Fewest statements paired for a finding.
const MIN_STATEMENTS: usize = 3;
/// Statement shapes seen in more blocks than this are boilerplate, not a sign of a copy.
const COMMON_SHAPE: usize = 40;

/// A block's statement as the pairing needs it: what it defines and reads.
struct Step<'a> {
    statement: &'a Statement,
    defines: Option<&'a str>,
    reads: BTreeSet<&'a str>,
}

/// Statements paired between two blocks: `(i, j)` for statement `i` of the
/// first and `j` of the second.
type Pairing = BTreeMap<usize, usize>;

/// Adds the reordered and interleaved copies not found already. Pairs of
/// functions with the same statements paired are one finding with a copy
/// in each function.
pub fn add(
    found: &mut Vec<CloneClass>,
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
    settings: Settings,
) {
    let known: BTreeSet<(String, String)> = found.iter().flat_map(function_pairs).collect();
    let steps: Vec<Vec<Step>> = blocks
        .iter()
        .map(|b| b.statements.iter().map(step).collect())
        .collect();
    let mut by_statements: BTreeMap<Vec<u64>, Vec<CloneClass>> = BTreeMap::new();
    for (a, b) in candidates(blocks) {
        let pairing = pair(&steps[a], &steps[b]);
        if !worth_reporting(&pairing, &steps[a], settings) || !out_of_order(&pairing) {
            continue;
        }
        let class = class(&pairing, (a, b), blocks, sources, display);
        if function_pairs(&class).any(|p| known.contains(&p)) {
            continue;
        }
        let mut shapes: Vec<u64> = pairing
            .keys()
            .map(|&i| blocks[a].statements[i].shape)
            .collect();
        shapes.sort_unstable();
        by_statements.entry(shapes).or_default().push(class);
    }
    let mut merged: Vec<CloneClass> = by_statements.into_values().flat_map(merge).collect();
    // A function pair found at two levels (a body and a block in it) keeps the bigger finding.
    merged.sort_by_key(|c| std::cmp::Reverse(c.statements));
    let mut seen = BTreeSet::new();
    merged.retain(|c| {
        let fresh: Vec<bool> = function_pairs(c).map(|p| seen.insert(p)).collect();
        fresh.contains(&true)
    });
    found.extend(merged);
}

/// Pairwise findings of the same statements as classes: those sharing a
/// function are one class, with each function's copy once.
fn merge(pairs: Vec<CloneClass>) -> Vec<CloneClass> {
    let key = |c: &Copy| (c.file.clone(), c.function.clone());
    let mut classes: Vec<CloneClass> = Vec::new();
    for pair in pairs {
        let joins = classes.iter().position(|c| {
            c.copies
                .iter()
                .any(|x| pair.copies.iter().any(|y| key(x) == key(y)))
        });
        match joins {
            Some(n) => {
                let class = &mut classes[n];
                for copy in pair.copies {
                    if !class.copies.iter().any(|x| key(x) == key(&copy)) {
                        class.copies.push(copy);
                    }
                }
                class.cautions.extend(pair.cautions);
                class.cautions.dedup();
            }
            None => classes.push(pair),
        }
    }
    for class in &mut classes {
        class
            .copies
            .sort_by(|a, b| (&a.file, a.start_line).cmp(&(&b.file, b.start_line)));
        let lines: i64 = class.copies[1..]
            .iter()
            .map(|c| {
                c.parts
                    .iter()
                    .map(|p| (p[1] + 1 - p[0]) as i64)
                    .sum::<i64>()
            })
            .sum();
        class.saves = lines - 2 * (class.copies.len() as i64 - 1);
    }
    classes
}

/// Every pair of functions a class has copies in, by file and name.
fn function_pairs(class: &CloneClass) -> impl Iterator<Item = (String, String)> + '_ {
    let name = |c: &Copy| format!("{}:{}", c.file, c.function.as_deref().unwrap_or(""));
    class.copies.iter().enumerate().flat_map(move |(n, a)| {
        class.copies[n + 1..].iter().map(move |b| {
            let (x, y) = (name(a), name(b));
            if x <= y {
                (x, y)
            } else {
                (y, x)
            }
        })
    })
}

/// What a statement defines (the name assigned before its `=`, or changed
/// by `+=` or `++`) and reads (every other name in it).
fn step(statement: &Statement) -> Step<'_> {
    let tokens = &statement.tokens;
    let assigns = tokens.iter().position(|t| {
        t.kind == Kind::Other
            && matches!(
                t.text.as_str(),
                "=" | "+=" | "-=" | "*=" | "/=" | "++" | "--"
            )
    });
    let defines = assigns.and_then(|at| {
        tokens[..at]
            .iter()
            .rev()
            .find(|t| t.kind == Kind::Name)
            .map(|t| t.text.as_str())
    });
    let changes = assigns.is_some_and(|at| tokens[at].text != "=");
    let reads = tokens
        .iter()
        .filter(|t| t.kind == Kind::Name && (changes || Some(t.text.as_str()) != defines))
        .map(|t| t.text.as_str())
        .collect();
    Step {
        statement,
        defines,
        reads,
    }
}

/// Pairs of blocks, by index, with at least `MIN_STATEMENTS` statement
/// shapes in common, rare shapes only, in different functions.
fn candidates(blocks: &[Block]) -> Vec<(usize, usize)> {
    let mut by_shape: HashMap<u64, Vec<usize>> = HashMap::new();
    for (n, block) in blocks.iter().enumerate() {
        let shapes: BTreeSet<u64> = block.statements.iter().map(|s| s.shape).collect();
        for shape in shapes {
            by_shape.entry(shape).or_default().push(n);
        }
    }
    let mut shared: HashMap<(usize, usize), usize> = HashMap::new();
    for owners in by_shape
        .values()
        .filter(|o| o.len() > 1 && o.len() <= COMMON_SHAPE)
    {
        for (k, &a) in owners.iter().enumerate() {
            for &b in &owners[k + 1..] {
                *shared.entry((a, b)).or_default() += 1;
            }
        }
    }
    let mut pairs: Vec<(usize, usize)> = shared
        .into_iter()
        .filter(|&((a, b), n)| n >= MIN_STATEMENTS && other_functions(&blocks[a], &blocks[b]))
        .map(|(pair, _)| pair)
        .collect();
    pairs.sort_unstable();
    pairs
}

/// Two blocks of different functions, or of none: a block and the blocks
/// nested in it are not copies of each other.
fn other_functions(a: &Block, b: &Block) -> bool {
    a.file != b.file || a.function.is_none() || a.function != b.function
}

/// The statements of `a` paired with statements of `b`: by shape and one
/// renaming, first come first, then with every pair whose data flow differs
/// taken out, until none is left to take out.
fn pair(a: &[Step], b: &[Step]) -> Pairing {
    let mut renaming: HashMap<&str, &str> = HashMap::new();
    let mut taken = BTreeSet::new();
    let mut pairing = Pairing::new();
    for (i, x) in a.iter().enumerate() {
        let partner = (0..b.len()).find(|&j| {
            !taken.contains(&j)
                && b[j].statement.shape == x.statement.shape
                && renames(x, &b[j], &renaming)
        });
        if let Some(j) = partner {
            record(x, &b[j], &mut renaming);
            taken.insert(j);
            pairing.insert(i, j);
        }
    }
    while let Some(wrong) = pairing
        .iter()
        .find(|&(&i, &j)| !same_flow(i, j, a, b, &pairing, &renaming))
    {
        let i = *wrong.0;
        pairing.remove(&i);
    }
    connected(pairing, a)
}

/// The largest group of paired statements linked by what they define and
/// read: one computation. Statements that only look alike (a list of
/// independent calls, say) are not a copy of anything.
fn connected(pairing: Pairing, a: &[Step]) -> Pairing {
    let paired: Vec<usize> = pairing.keys().copied().collect();
    let mut group: HashMap<usize, usize> = paired.iter().map(|&i| (i, i)).collect();
    fn root(group: &mut HashMap<usize, usize>, i: usize) -> usize {
        let up = group[&i];
        if up == i {
            return i;
        }
        let top = root(group, up);
        group.insert(i, top);
        top
    }
    for &i in &paired {
        for name in &a[i].reads {
            if let Some(d) = last_definition(a, i, name).filter(|d| pairing.contains_key(d)) {
                let (x, y) = (root(&mut group, d), root(&mut group, i));
                group.insert(x, y);
            }
        }
    }
    let mut sizes: HashMap<usize, usize> = HashMap::new();
    for &i in &paired {
        *sizes.entry(root(&mut group, i)).or_default() += 1;
    }
    let Some((&largest, _)) = sizes
        .iter()
        .max_by_key(|&(&r, &n)| (n, std::cmp::Reverse(r)))
    else {
        return pairing;
    };
    pairing
        .into_iter()
        .filter(|&(i, _)| root(&mut group, i) == largest)
        .collect()
}

/// Whether the names of `x` map to those of `y` without breaking the
/// renaming so far. Only locals and parameters may be renamed: members
/// and called functions are the same, or the statements do other things.
fn renames(x: &Step, y: &Step, renaming: &HashMap<&str, &str>) -> bool {
    let (from, to) = (names(x), names(y));
    from.len() == to.len()
        && from.iter().zip(&to).all(|(&(f, fixed), &(t, _))| {
            (!fixed || f == t)
                && renaming.get(f).is_none_or(|known| *known == t)
                && renaming.iter().all(|(k, v)| *v != t || *k == f)
        })
}

/// The names in a statement, in order, each with whether it is a member
/// or a function called (after a `.`, or before a `(`), which keeps its name.
fn names<'a>(step: &Step<'a>) -> Vec<(&'a str, bool)> {
    let tokens = &step.statement.tokens;
    let text = |n: usize| tokens.get(n).map(|t| t.text.as_str());
    (0..tokens.len())
        .filter(|&n| tokens[n].kind == Kind::Name)
        .map(|n| {
            let member = n > 0 && matches!(text(n - 1), Some("." | "?." | "::"));
            (tokens[n].text.as_str(), member || text(n + 1) == Some("("))
        })
        .collect()
}

fn record<'a>(x: &Step<'a>, y: &Step<'a>, renaming: &mut HashMap<&'a str, &'a str>) {
    for ((f, _), (t, _)) in names(x).into_iter().zip(names(y)) {
        renaming.insert(f, t);
    }
}

/// Whether each name `a[i]` reads comes from where its partner's does:
/// from partnered statements, or from outside the copy in both.
fn same_flow(
    i: usize,
    j: usize,
    a: &[Step],
    b: &[Step],
    pairing: &Pairing,
    renaming: &HashMap<&str, &str>,
) -> bool {
    a[i].reads.iter().all(|name| {
        let from_a = last_definition(a, i, name);
        let from_b = renaming.get(name).and_then(|n| last_definition(b, j, n));
        match (
            from_a.filter(|d| pairing.contains_key(d)),
            from_b.filter(|d| pairing.values().any(|v| v == d)),
        ) {
            (Some(d), Some(e)) => pairing.get(&d) == Some(&e),
            (None, None) => true,
            _ => false,
        }
    })
}

/// The last statement before `before` that defines `name`.
fn last_definition(steps: &[Step], before: usize, name: &str) -> Option<usize> {
    (0..before).rev().find(|&k| steps[k].defines == Some(name))
}

fn worth_reporting(pairing: &Pairing, a: &[Step], settings: Settings) -> bool {
    let tokens: usize = pairing.keys().map(|&i| a[i].statement.tokens.len()).sum();
    let lines: usize = pairing
        .keys()
        .map(|&i| a[i].statement.end_line + 1 - a[i].statement.start_line)
        .sum();
    pairing.len() >= MIN_STATEMENTS && tokens >= settings.min_tokens && lines >= settings.min_lines
}

/// The paired statements are in another order, or have others between
/// them in either block: what the in-order passes do not find.
fn out_of_order(pairing: &Pairing) -> bool {
    let firsts: Vec<usize> = pairing.keys().copied().collect();
    let seconds: Vec<usize> = pairing.values().copied().collect();
    let gaps = |xs: &[usize]| xs.windows(2).any(|w| w[1] != w[0] + 1);
    !seconds.windows(2).all(|w| w[0] < w[1]) || gaps(&firsts) || {
        let mut sorted = seconds.clone();
        sorted.sort_unstable();
        gaps(&sorted)
    }
}

/// The finding for two blocks: a copy in each, the paired lines as its
/// parts, and what sits between them.
fn class(
    pairing: &Pairing,
    (a, b): (usize, usize),
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
) -> CloneClass {
    let first: Vec<usize> = pairing.keys().copied().collect();
    let mut second: Vec<usize> = pairing.values().copied().collect();
    second.sort_unstable();
    let copies = vec![
        copy(&blocks[a], &first, sources, display),
        copy(&blocks[b], &second, sources, display),
    ];
    let reordered = !pairing
        .values()
        .collect::<Vec<_>>()
        .windows(2)
        .all(|w| w[0] < w[1]);
    let lines: usize = copies[1].parts.iter().map(|p| p[1] + 1 - p[0]).sum();
    let cautions = [
        between(&blocks[a], &first, &copies[0]),
        between(&blocks[b], &second, &copies[1]),
    ]
    .into_iter()
    .flatten()
    .collect();
    CloneClass {
        id: String::new(),
        kind: "copies",
        clone_type: 3,
        evidence: "dependence",
        similarity: None,
        lines,
        tokens: first
            .iter()
            .map(|&i| blocks[a].statements[i].tokens.len())
            .sum(),
        statements: first.len(),
        template: template(&blocks[a], &first, &sources[blocks[a].file].text),
        holes: Vec::new(),
        saves: lines as i64 - 2,
        unite: unite(reordered, first.len()),
        cautions,
        differences: Vec::new(),
        rules: Vec::new(),
        entity: None,
        confirmation: None,
        risk: None,
        signature: None,
        copies,
    }
}

fn unite(reordered: bool, statements: usize) -> String {
    let how = if reordered {
        "in another order"
    } else {
        "with other statements between them"
    };
    format!("the same {statements} statements compute the same values in both, {how}: gather them (the data flow allows it), then extract them into one function and call it from both")
}

/// The block's copy: from its first paired statement to its last, with the paired lines as parts.
fn copy(
    block: &Block,
    paired: &[usize],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
) -> Copy {
    let statements = &block.statements;
    let (start, end) = (
        statements[paired[0]].start_line,
        statements[paired[paired.len() - 1]].end_line,
    );
    Copy {
        file: display(&sources[block.file].path),
        start_line: start,
        end_line: end,
        function: block.function.clone(),
        changed: false,
        code: excerpt(&sources[block.file].text, start, end),
        parts: parts(
            paired
                .iter()
                .map(|&i| [statements[i].start_line, statements[i].end_line]),
        ),
    }
}

/// Line ranges, those that touch joined.
fn parts(lines: impl Iterator<Item = [usize; 2]>) -> Vec<[usize; 2]> {
    let mut joined: Vec<[usize; 2]> = Vec::new();
    for [start, end] in lines {
        match joined.last_mut() {
            Some(last) if start <= last[1] + 1 => last[1] = last[1].max(end),
            _ => joined.push([start, end]),
        }
    }
    joined
}

/// The paired statements of the first copy, `...` where others sit between.
fn template(block: &Block, paired: &[usize], text: &str) -> String {
    let mut out = Vec::new();
    for (n, &i) in paired.iter().enumerate() {
        if n > 0 && i != paired[n - 1] + 1 {
            out.push("...".to_string());
        }
        let s = &block.statements[i];
        out.push(text[s.start_byte..s.end_byte].to_string());
    }
    out.join("\n")
}

/// Statements between the paired ones that are not part of the copy.
fn between(block: &Block, paired: &[usize], copy: &Copy) -> Option<String> {
    let others: Vec<String> = (paired[0]..=paired[paired.len() - 1])
        .filter(|i| !paired.contains(i))
        .map(|i| block.statements[i].start_line.to_string())
        .collect();
    (!others.is_empty()).then(|| {
        format!(
            "in {}, lines {} sit between and are not part of it: move them before or after it where they do not use what it computes",
            copy.function.as_deref().unwrap_or(&copy.file),
            others.join(", ")
        )
    })
}
