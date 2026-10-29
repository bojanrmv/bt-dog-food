//! Near-miss copies (Type 3): function bodies that are alike apart from a few
//! tokens added, removed or changed, such as a copy with one more check.
//!
//! Every body big enough is a unit. Units that share enough runs of
//! normalized tokens are candidates; each candidate pair is aligned token by
//! token (a shortest edit script), and pairs whose matched tokens reach
//! `SIMILARITY` of both are near misses. Connected pairs form a class around
//! the copy most like the others, which every other copy is compared with:
//! the template marks what not all copies share, and the differences say
//! what each copy adds, removes or changes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::find::{
    declarations, describe, hole_kind, CloneClass, Difference, Hole, HoleKind, Settings, Span,
};
use crate::tokens::{fnv, Block, Kind, Source, Token};

/// Fewest matched tokens, as a share of both copies, for a near miss.
pub const SIMILARITY: f64 = 0.8;
/// Fewest tokens in a body compared whole. Lower than for a copy inside a
/// body: a whole function copied is worth uniting even when short.
const MIN_UNIT_TOKENS: usize = 25;
/// Most tokens in a body compared whole; bigger bodies are left to the
/// statement copies.
const MAX_UNIT_TOKENS: usize = 4000;
/// Tokens in a run used to find candidates.
const GRAM: usize = 8;
/// A run of tokens found in more bodies than this is boilerplate: it says
/// nothing about which bodies are alike.
const COMMON: usize = 64;

struct Unit<'b> {
    block: usize,
    tokens: Vec<&'b Token>,
    shapes: Vec<u64>,
    /// Each statement's shape and where its tokens start in `tokens`.
    statements: Vec<(u64, usize)>,
}

/// Fewest matched tokens, as a share of both, for two statements that
/// differ to count as one changed statement rather than one replaced.
const LOCAL_SIMILARITY: f64 = 0.7;

/// Adds near-miss classes of whole bodies to `found`, and whole bodies too
/// short for the statement copies. A class of copies that lies inside the
/// bodies of a near-miss class is dropped: uniting the bodies covers it.
pub(crate) fn add(
    found: &mut Vec<CloneClass>,
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
    settings: Settings,
) {
    let units = units(blocks, settings);
    let pairs = similar_pairs(&units, blocks, sources);
    let mut near: Vec<CloneClass> = Vec::new();
    for mut members in components(units.len(), &pairs) {
        // Pairs chain: what is not near the center forms classes of its own.
        while members.len() > 1 {
            let (class, taken) = class_of(&members, &pairs, &units, blocks, sources, display);
            match class.filter(|c| c.saves > 0 && !crate::find::mostly_parameters(c)) {
                Some(class) => {
                    near.push(class);
                    members.retain(|m| !taken.contains(m));
                }
                // Not worth reporting: try again without the copy least like
                // the center, or without the center once it has one left.
                None => {
                    let drop = if taken.len() > 2 {
                        taken.last()
                    } else {
                        taken.first()
                    };
                    let drop = drop.copied();
                    members.retain(|m| Some(*m) != drop);
                }
            }
        }
    }
    let place = |c: &crate::Copy| (c.file.clone(), c.start_line, c.end_line);
    let inside = |inner: &crate::Copy, outer: &crate::Copy| {
        inner.file == outer.file
            && outer.start_line <= inner.start_line
            && inner.end_line <= outer.end_line
    };
    // A near-miss class that only repeats a class of copies already found.
    let known: BTreeSet<Vec<(String, usize, usize)>> = found
        .iter()
        .map(|c| {
            let mut places: Vec<_> = c.copies.iter().map(place).collect();
            places.sort();
            places
        })
        .collect();
    near.retain(|c| {
        let mut places: Vec<_> = c.copies.iter().map(place).collect();
        places.sort();
        !known.contains(&places)
    });
    // An exact copy inside near-miss bodies: when it is most of each body,
    // extracting it is the cleaner refactor and the near miss adds nothing;
    // otherwise uniting the bodies covers it.
    let lines = |c: &crate::Copy| c.end_line + 1 - c.start_line;
    let within = |exact: &CloneClass, n: &CloneClass| {
        exact
            .copies
            .iter()
            .all(|copy| n.copies.iter().any(|body| inside(copy, body)))
    };
    let most_of = |exact: &CloneClass, n: &CloneClass| {
        n.copies.iter().all(|body| {
            exact
                .copies
                .iter()
                .any(|copy| inside(copy, body) && 10 * lines(copy) >= 7 * lines(body))
        })
    };
    near.retain(|n| {
        n.clone_type != 3
            || !found
                .iter()
                .any(|exact| within(exact, n) && most_of(exact, n))
    });
    found.retain(|exact| {
        !near
            .iter()
            .any(|n| n.clone_type == 3 && within(exact, n) && !most_of(exact, n))
    });
    found.extend(near);
}

/// Function bodies big enough to compare whole.
fn units(blocks: &[Block], settings: Settings) -> Vec<Unit<'_>> {
    blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.is_body && !b.is_constructor && b.function.is_some())
        .filter_map(|(n, b)| {
            let tokens: Vec<&Token> = b.statements.iter().flat_map(|s| &s.tokens).collect();
            let mut start = 0;
            let statements = b
                .statements
                .iter()
                .map(|s| {
                    start += s.tokens.len();
                    (s.shape, start - s.tokens.len())
                })
                .collect();
            let lines = b.statements.last()?.end_line + 1 - b.statements.first()?.start_line;
            let size = tokens.len();
            (lines >= settings.min_lines && (MIN_UNIT_TOKENS..=MAX_UNIT_TOKENS).contains(&size))
                .then(|| Unit {
                    block: n,
                    shapes: tokens.iter().map(|t| t.shape()).collect(),
                    tokens,
                    statements,
                })
        })
        .collect()
}

/// Positions `(i, j)` where the tokens of two units match.
type Matches = Vec<(usize, usize)>;

/// Pairs of units (by index) that are near misses: their similarity and
/// where their tokens match.
type Pairs = BTreeMap<(usize, usize), (f64, Matches)>;

/// A unit compared with a class's center: its index, similarity, matches.
type Member = (usize, f64, Matches);

fn similar_pairs(units: &[Unit], blocks: &[Block], sources: &[Source]) -> Pairs {
    let grams: Vec<Vec<u64>> = units
        .iter()
        .map(|u| {
            let set: BTreeSet<u64> = u
                .shapes
                .windows(GRAM)
                .map(|w| {
                    let bytes: Vec<[u8; 8]> = w.iter().map(|s| s.to_le_bytes()).collect();
                    let parts: Vec<&[u8]> = bytes.iter().map(|b| b.as_slice()).collect();
                    fnv(&parts)
                })
                .collect();
            set.into_iter().collect()
        })
        .collect();
    let mut index: HashMap<u64, Vec<usize>> = HashMap::new();
    for (unit, set) in grams.iter().enumerate() {
        for gram in set {
            index.entry(*gram).or_default().push(unit);
        }
    }
    let mut shared: HashMap<(usize, usize), usize> = HashMap::new();
    for list in index.values().filter(|l| l.len() > 1 && l.len() <= COMMON) {
        for (n, a) in list.iter().enumerate() {
            for b in &list[n + 1..] {
                *shared.entry((*a, *b)).or_default() += 1;
            }
        }
    }
    let language = |u: &Unit| {
        sources[blocks[u.block].file]
            .path
            .extension()
            .map(|e| e.to_ascii_lowercase())
    };
    let mut pairs = Pairs::new();
    for ((a, b), count) in shared {
        let (ua, ub) = (&units[a], &units[b]);
        let (n, m) = (ua.shapes.len(), ub.shapes.len());
        // Similarity is at most 2·min / (n + m).
        let enough_grams = count * 10 >= 3 * grams[a].len().min(grams[b].len());
        let close_sizes = (2 * n.min(m)) as f64 >= SIMILARITY * (n + m) as f64;
        if !enough_grams || !close_sizes || language(ua) != language(ub) {
            continue;
        }
        if let Some((similarity, matches)) = compare(ua, ub) {
            pairs.insert((a, b), (similarity, matches));
        }
    }
    pairs
}

/// The similarity of two token sequences and where they match, when it
/// reaches `SIMILARITY`.
///
/// Statements are aligned first, by shape; tokens are aligned only between
/// the statements that differ, and only where those are alike enough to be
/// one statement changed. So two unrelated statements in the same place do
/// not lend each other a `return` and a `;`.
fn compare(a: &Unit, b: &Unit) -> Option<(f64, Vec<(usize, usize)>)> {
    let total = a.shapes.len() + b.shapes.len();
    let shapes = |u: &Unit| u.statements.iter().map(|s| s.0).collect::<Vec<u64>>();
    let (sa, sb) = (shapes(a), shapes(b));
    let statements = align(&sa, &sb, sa.len() + sb.len())?;
    // Token positions of statements `from..to` of a unit.
    let range = |u: &Unit, from: usize, to: usize| {
        let at = |k: usize| u.statements.get(k).map_or(u.shapes.len(), |s| s.1);
        at(from)..at(to)
    };
    let mut matches = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (si, sj) in statements.iter().copied().chain([(sa.len(), sb.len())]) {
        matches.extend(changed_statements(a, b, i..si, j..sj, &range));
        if si < sa.len() {
            let (ma, mb) = (range(a, si, si + 1), range(b, sj, sj + 1));
            matches.extend(ma.zip(mb));
        }
        (i, j) = (si + 1, sj + 1);
    }
    let similarity = 2.0 * matches.len() as f64 / total as f64;
    (similarity >= SIMILARITY).then_some((similarity, matches))
}

/// Most statement pairs compared inside one gap between matched statements.
const GAP_PAIRS: usize = 400;

/// Token matches inside a gap between matched statements: its statements
/// paired in order where a pair is alike enough to be one statement changed
/// (most matched tokens overall), and tokens matched only within a pair.
fn changed_statements(
    a: &Unit,
    b: &Unit,
    ga: std::ops::Range<usize>,
    gb: std::ops::Range<usize>,
    range: &dyn Fn(&Unit, usize, usize) -> std::ops::Range<usize>,
) -> Vec<(usize, usize)> {
    let (n, m) = (ga.len(), gb.len());
    if n == 0 || m == 0 || n * m > GAP_PAIRS {
        return Vec::new();
    }
    let local = |x: usize, y: usize| -> Option<Vec<(usize, usize)>> {
        let (ra, rb) = (
            range(a, ga.start + x, ga.start + x + 1),
            range(b, gb.start + y, gb.start + y + 1),
        );
        let size = ra.len() + rb.len();
        let bound = ((1.0 - LOCAL_SIMILARITY) * size as f64).floor() as usize;
        let found = align(&a.shapes[ra.clone()], &b.shapes[rb.clone()], bound)?;
        let alike = 2.0 * found.len() as f64 >= LOCAL_SIMILARITY * size as f64;
        alike.then(|| {
            found
                .iter()
                .map(|(p, q)| (ra.start + p, rb.start + q))
                .collect()
        })
    };
    let pairs: Vec<Vec<Option<Matches>>> = (0..n)
        .map(|x| (0..m).map(|y| local(x, y)).collect())
        .collect();
    // best[x][y]: most tokens matched pairing statements x.. with y..
    let mut best = vec![vec![0usize; m + 1]; n + 1];
    for x in (0..n).rev() {
        for y in (0..m).rev() {
            let paired = pairs[x][y]
                .as_ref()
                .map_or(0, |p| p.len() + best[x + 1][y + 1]);
            best[x][y] = paired.max(best[x + 1][y]).max(best[x][y + 1]);
        }
    }
    let mut out = Vec::new();
    let (mut x, mut y) = (0, 0);
    while x < n && y < m {
        match &pairs[x][y] {
            Some(p) if best[x][y] == p.len() + best[x + 1][y + 1] => {
                out.extend(p.iter().copied());
                x += 1;
                y += 1;
            }
            _ if best[x][y] == best[x + 1][y] => x += 1,
            _ => y += 1,
        }
    }
    out
}

/// The positions `(i, j)` where `a[i] == b[j]` in a shortest edit script
/// from `a` to `b` (Myers' O(ND) algorithm), or `None` when it takes more
/// than `max_edits` insertions and deletions.
pub(crate) fn align(a: &[u64], b: &[u64], max_edits: usize) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = max_edits as isize;
    let offset = max + 1;
    let at = |k: isize| (k + offset) as usize;
    let mut v = vec![0isize; 2 * offset as usize + 1];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=max {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
                v[at(k + 1)]
            } else {
                v[at(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[at(k)] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m, offset));
            }
            k += 2;
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize, offset: isize) -> Vec<(usize, usize)> {
    let at = |k: isize| (k + offset) as usize;
    let mut matches = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let previous = if k == -d || (k != d && v[at(k - 1)] < v[at(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let px = v[at(previous)];
        let py = px - previous;
        while x > px && y > py && x > 0 && y > 0 {
            x -= 1;
            y -= 1;
            matches.push((x as usize, y as usize));
        }
        if d > 0 {
            x = px;
            y = py;
        }
    }
    matches.reverse();
    matches
}

/// Groups of units connected by near-miss pairs.
fn components(count: usize, pairs: &Pairs) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..count).collect();
    fn root(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for (a, b) in pairs.keys() {
        let (ra, rb) = (root(&mut parent, *a), root(&mut parent, *b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for unit in pairs
        .keys()
        .flat_map(|(a, b)| [*a, *b])
        .collect::<BTreeSet<_>>()
    {
        groups
            .entry(root(&mut parent, unit))
            .or_default()
            .push(unit);
    }
    groups.into_values().filter(|g| g.len() > 1).collect()
}

/// The class of a group of near-miss units: around the unit most like the
/// others, with every other unit that is a near miss of it; and the units it
/// took (the center always, so the rest of the group can go on without it).
fn class_of(
    members: &[usize],
    pairs: &Pairs,
    units: &[Unit],
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
) -> (Option<CloneClass>, Vec<usize>) {
    let pair = |a: usize, b: usize| -> Option<(f64, Vec<(usize, usize)>)> {
        if let Some((s, m)) = pairs.get(&(a.min(b), a.max(b))) {
            let m = if a < b {
                m.clone()
            } else {
                m.iter().map(|(i, j)| (*j, *i)).collect()
            };
            return Some((*s, m));
        }
        compare(&units[a], &units[b])
    };
    let Some(&center) = members.iter().max_by(|a, b| {
        let total = |u: usize| -> f64 {
            members
                .iter()
                .filter(|o| **o != u)
                .filter_map(|o| pairs.get(&(u.min(*o), u.max(*o))).map(|p| p.0))
                .sum()
        };
        total(**a).total_cmp(&total(**b)).then(b.cmp(a))
    }) else {
        return (None, Vec::new());
    };
    let mut others: Vec<Member> = members
        .iter()
        .filter(|o| **o != center)
        .filter_map(|o| pair(center, *o).map(|(s, m)| (*o, s, m)))
        .collect();
    // Most alike first: the last one taken is the weakest link.
    others.sort_by(|a, b| b.1.total_cmp(&a.1));
    let taken: Vec<usize> = std::iter::once(center)
        .chain(others.iter().map(|(o, _, _)| *o))
        .collect();
    if others.is_empty() {
        return (None, taken);
    }
    others.sort_by_key(|(o, _, _)| units[*o].block);
    let spans: Vec<Span> = std::iter::once(center)
        .chain(others.iter().map(|(o, _, _)| *o))
        .map(|u| whole(&blocks[units[u].block], units[u].block))
        .collect();
    let exact = others.iter().all(|(o, _, m)| {
        m.len() == units[center].shapes.len() && m.len() == units[*o].shapes.len()
    });
    if exact {
        return (Some(describe(&spans, blocks, sources, display)), taken);
    }
    let class = near_class(center, &others, &spans, units, blocks, sources, display);
    (Some(class), taken)
}

fn whole(block: &Block, index: usize) -> Span {
    Span {
        block: index,
        first: 0,
        last: block.statements.len() - 1,
    }
}

/// A class of near misses: the center's text as the template, with holes
/// where every copy has a token there but not the same one, and `⟪…⟫`
/// around what not every copy has.
fn near_class(
    center: usize,
    others: &[Member],
    spans: &[Span],
    units: &[Unit],
    blocks: &[Block],
    sources: &[Source],
    display: &dyn Fn(&std::path::Path) -> String,
) -> CloneClass {
    let first = &units[center].tokens;
    // For each token of the center, the token each other copy has there.
    let mapped: Vec<Vec<Option<usize>>> = others
        .iter()
        .map(|(_, _, matches)| {
            let mut at = vec![None; first.len()];
            for (i, j) in matches {
                at[*i] = Some(*j);
            }
            at
        })
        .collect();
    let shared = |p: usize| mapped.iter().all(|m| m[p].is_some());
    let declared = declarations(first);
    let mut holes: Vec<Hole> = Vec::new();
    let mut by_values: BTreeMap<Vec<String>, usize> = BTreeMap::new();
    let mut hole_at: BTreeMap<usize, usize> = BTreeMap::new();
    for p in 0..first.len() {
        if first[p].kind == Kind::Other || !shared(p) {
            continue;
        }
        let values: Vec<String> = std::iter::once(first[p].text.clone())
            .chain(
                others
                    .iter()
                    .zip(&mapped)
                    .map(|((o, _, _), m)| units[*o].tokens[m[p].expect("shared")].text.clone()),
            )
            .collect();
        if values.iter().all(|v| *v == values[0]) {
            continue;
        }
        let index = *by_values.entry(values.clone()).or_insert_with(|| {
            holes.push(Hole {
                index: holes.len() + 1,
                kind: hole_kind(first, p),
                values,
            });
            holes.len()
        });
        if declared.contains(&p) {
            holes[index - 1].kind = HoleKind::Local;
        }
        hole_at.insert(p, index);
    }
    let source = &sources[blocks[units[center].block].file].text;
    let start = first[0].start_byte;
    let end = first[first.len() - 1].end_byte;
    let mut template = String::new();
    let mut cursor = start;
    let mut p = 0;
    while p < first.len() {
        if !shared(p) {
            let mut q = p;
            while q + 1 < first.len() && !shared(q + 1) {
                q += 1;
            }
            template.push_str(&source[cursor..first[p].start_byte]);
            template.push('⟪');
            template.push_str(&source[first[p].start_byte..first[q].end_byte]);
            template.push('⟫');
            cursor = first[q].end_byte;
            p = q + 1;
            continue;
        }
        if let Some(index) = hole_at.get(&p) {
            template.push_str(&source[cursor..first[p].start_byte]);
            template.push_str(&format!("⟨{index}⟩"));
            cursor = first[p].end_byte;
        }
        p += 1;
    }
    template.push_str(&source[cursor..end]);
    let mut class = describe(&spans[..1], blocks, sources, display);
    let copies: Vec<crate::Copy> = spans
        .iter()
        .map(|s| describe(&[*s], blocks, sources, display).copies.remove(0))
        .collect();
    let differences: Vec<Difference> = others
        .iter()
        .enumerate()
        .flat_map(|(n, (o, _, matches))| {
            differences(n + 1, first, &units[*o].tokens, matches, source, {
                &sources[blocks[units[*o].block].file].text
            })
        })
        .collect();
    let similarity = others.iter().map(|(_, s, _)| *s).fold(1.0, f64::min);
    let lines = class.lines;
    let parameters = holes.iter().filter(|h| h.kind != HoleKind::Local).count();
    let edits = differences.len();
    class.saves = ((copies.len() - 1) * lines) as i64
        - copies.len() as i64
        - 2
        - (parameters / 3) as i64
        - edits as i64;
    class.unite = format!(
        "the functions' bodies are alike apart from {edits} {}: keep one function and pass what differs as a parameter, a flag or a delegate (marked ⟪…⟫, and in the `differs` lines), or move the shared steps into one helper they all call{}",
        if edits == 1 { "edit" } else { "edits" },
        if holes.iter().any(|h| h.kind != HoleKind::Local) {
            format!("; the shared parts differ in {}", holes_list(&holes))
        } else {
            String::new()
        }
    );
    class.copies = copies;
    class.clone_type = 3;
    class.similarity = Some((similarity * 100.0).round() / 100.0);
    class.template = crate::find::dedent(
        &template,
        start - source[..start].rfind('\n').map_or(0, |n| n + 1),
    );
    class.holes = holes;
    class.differences = differences;
    class.cautions.clear();
    class
}

fn holes_list(holes: &[Hole]) -> String {
    holes
        .iter()
        .filter(|h| h.kind != HoleKind::Local)
        .map(|h| format!("⟨{}⟩ ({})", h.index, h.kind.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What `copy` (the `n`th copy of the class) adds, removes or changes
/// against the center, from the tokens left between their matches.
fn differences(
    n: usize,
    center: &[&Token],
    other: &[&Token],
    matches: &[(usize, usize)],
    center_text: &str,
    other_text: &str,
) -> Vec<Difference> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (mi, mj) in matches.iter().copied().chain([(center.len(), other.len())]) {
        if i < mi || j < mj {
            let text = |tokens: &[&Token], from: usize, to: usize, source: &str| -> String {
                if from >= to {
                    return String::new();
                }
                let raw = &source[tokens[from].start_byte..tokens[to - 1].end_byte];
                raw.split_whitespace().collect::<Vec<_>>().join(" ")
            };
            let line_of = |tokens: &[&Token], at: usize, source: &str| {
                let byte = tokens.get(at).or(tokens.last()).map_or(0, |t| t.start_byte);
                source[..byte].matches('\n').count() + 1
            };
            let first = text(center, i, mi, center_text);
            let second = text(other, j, mj, other_text);
            out.push(Difference {
                copy: n,
                kind: match (first.is_empty(), second.is_empty()) {
                    (true, _) => "added",
                    (_, true) => "removed",
                    _ => "changed",
                },
                line: line_of(other, j, other_text),
                first,
                other: second,
            });
        }
        i = mi + 1;
        j = mj + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_finds_the_longest_common_subsequence() {
        let a = [1, 2, 3, 4, 5, 6];
        let b = [1, 2, 9, 4, 5, 6, 7];
        let matches = align(&a, &b, 10).unwrap();
        assert_eq!(matches, [(0, 0), (1, 1), (3, 3), (4, 4), (5, 5)]);
        assert!(align(&a, &b, 2).is_none(), "three edits do not fit in two");
        assert_eq!(align(&a, &a, 0).unwrap().len(), 6);
        assert_eq!(align(&[], &[1, 2], 2).unwrap(), []);
    }
}
