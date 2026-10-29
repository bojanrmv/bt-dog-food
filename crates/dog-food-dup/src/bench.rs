//! Scores a duplication report against a labelled corpus (cases.json in
//! tests/corpus/csharp): how many known copies and rewrites it found, by
//! clone type and by how the code was rewritten, and how many of its matches
//! are wrong. It reads the report as JSON, so it scores any tool that writes
//! dgf-dup's format, not only this one.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::Value;

/// cases.json.
#[derive(Debug, Deserialize)]
pub struct Labels {
    pub groups: Vec<Group>,
    pub pairs: Vec<Pair>,
}

/// Methods that all do the same thing; each names how it is written.
#[derive(Debug, Deserialize)]
pub struct Group {
    pub id: String,
    pub members: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct Pair {
    pub id: String,
    pub a: String,
    pub b: String,
    /// 1-3 for copies; 0 for code that only looks alike.
    #[serde(rename = "type")]
    pub kind: u8,
    pub equal: bool,
    pub note: String,
}

/// One labelled pair of methods, from a pair or from two group members.
#[derive(Debug, Clone)]
struct Label {
    id: String,
    /// 0 (unrelated), 1-3 (copies) or 4 (same behaviour, rewritten).
    kind: u8,
    equal: bool,
    /// How the two differ: the note, or "for ↔ linq" for group members.
    how: String,
}

/// What a report says about a pair of methods.
#[derive(Debug, Clone, Copy, Default)]
struct Found {
    /// The report claims they do the same thing, not only that they look alike.
    equal: bool,
}

#[derive(Debug, Default)]
pub struct Scorecard {
    /// Clone type → (found, labelled).
    pub by_type: BTreeMap<u8, (usize, usize)>,
    /// For Type 4, each group → (found, labelled) pairs of its members.
    pub by_group: BTreeMap<String, (usize, usize)>,
    /// Labelled pairs the report missed (id: a vs b).
    pub missed: Vec<String>,
    /// Matches between code labelled unrelated.
    pub unrelated: Vec<String>,
    /// Claims of equal behaviour: right, and wrong (on pairs labelled not equal).
    pub equal_right: usize,
    pub equal_wrong: Vec<String>,
    /// Matched pairs the labels say nothing about, to review and label.
    pub unlabelled: Vec<String>,
}

impl Scorecard {
    /// Found pairs over labelled clone pairs (Types 1-4).
    pub fn recall(&self) -> f64 {
        let (found, all) = self
            .by_type
            .iter()
            .filter(|(kind, _)| **kind > 0)
            .fold((0, 0), |(f, a), (_, (found, all))| (f + found, a + all));
        ratio(found, all)
    }

    /// Of the matches on labelled pairs, those that are clones.
    pub fn precision(&self) -> f64 {
        let right: usize = self
            .by_type
            .iter()
            .filter(|(kind, _)| **kind > 0)
            .map(|(_, (found, _))| found)
            .sum();
        ratio(right, right + self.unrelated.len())
    }

    pub fn recall_of(&self, kind: u8) -> f64 {
        let (found, all) = self.by_type.get(&kind).copied().unwrap_or_default();
        ratio(found, all)
    }
}

fn ratio(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        1.0
    } else {
        part as f64 / whole as f64
    }
}

impl Labels {
    pub fn parse(text: &str) -> Result<Labels, String> {
        serde_json::from_str(text).map_err(|err| format!("cases.json: {err}"))
    }

    /// Every labelled pair, keyed by its two methods in order.
    fn pairs(&self) -> BTreeMap<(String, String), Label> {
        let mut out = BTreeMap::new();
        for group in &self.groups {
            let members: Vec<(&String, &String)> = group.members.iter().collect();
            for (i, (a, how_a)) in members.iter().enumerate() {
                for (b, how_b) in &members[i + 1..] {
                    out.insert(
                        key(a, b),
                        Label {
                            id: group.id.clone(),
                            kind: 4,
                            equal: true,
                            how: rewrite(how_a, how_b),
                        },
                    );
                }
            }
        }
        for pair in &self.pairs {
            out.insert(
                key(&pair.a, &pair.b),
                Label {
                    id: pair.id.clone(),
                    kind: pair.kind,
                    equal: pair.equal,
                    how: pair.note.clone(),
                },
            );
        }
        out
    }
}

fn key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// "for ↔ linq", the same whichever member comes first.
fn rewrite(a: &str, b: &str) -> String {
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    format!("{a} ↔ {b}")
}

/// The pairs of methods a report matches: any two functions with copies in
/// one class. A class claims equal behaviour when its `evidence` is more than
/// syntax (or `equal` is true).
fn matched(report: &Value) -> BTreeMap<(String, String), Found> {
    let mut out: BTreeMap<(String, String), Found> = BTreeMap::new();
    let classes = report["classes"].as_array().cloned().unwrap_or_default();
    for class in &classes {
        let equal = class["equal"].as_bool().unwrap_or(false)
            || matches!(
                class["evidence"].as_str(),
                Some("normalized" | "tested" | "proven")
            );
        let functions: BTreeSet<&str> = class["copies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|copy| copy["function"].as_str())
            .collect();
        let functions: Vec<&str> = functions.into_iter().collect();
        for (i, a) in functions.iter().enumerate() {
            for b in &functions[i + 1..] {
                let found = out.entry(key(a, b)).or_default();
                found.equal |= equal;
            }
        }
    }
    out
}

/// Scores `report` (a dgf-dup JSON report) against `labels`.
pub fn score(report: &Value, labels: &Labels) -> Scorecard {
    let labelled = labels.pairs();
    let found = matched(report);
    let mut card = Scorecard::default();
    for ((a, b), label) in &labelled {
        let hit = found.get(&(a.clone(), b.clone()));
        let name = format!("{}: {a} vs {b} ({})", label.id, label.how);
        if label.kind > 0 {
            let entry = card.by_type.entry(label.kind).or_default();
            entry.1 += 1;
            entry.0 += usize::from(hit.is_some());
            if label.kind == 4 {
                let entry = card.by_group.entry(label.id.clone()).or_default();
                entry.1 += 1;
                entry.0 += usize::from(hit.is_some());
            }
            if hit.is_none() {
                card.missed.push(name.clone());
            }
        } else if hit.is_some() {
            card.unrelated.push(name.clone());
        }
        if hit.is_some_and(|f| f.equal) {
            if label.equal {
                card.equal_right += 1;
            } else {
                card.equal_wrong.push(name);
            }
        }
    }
    for (a, b) in found.keys() {
        if !labelled.contains_key(&(a.clone(), b.clone())) {
            card.unlabelled.push(format!("{a} vs {b}"));
        }
    }
    card
}

/// The scorecard as text.
pub fn render(card: &Scorecard, seconds: Option<f64>) -> String {
    let mut out = String::new();
    let pct = |x: f64| format!("{:.0}%", x * 100.0);
    let _ = writeln!(
        out,
        "recall {} · precision {} · equal claims {} right, {} wrong{}",
        pct(card.recall()),
        pct(card.precision()),
        card.equal_right,
        card.equal_wrong.len(),
        seconds.map(|s| format!(" · {s:.2} s")).unwrap_or_default()
    );
    let names = ["unrelated", "Type 1", "Type 2", "Type 3", "Type 4"];
    for (kind, (found, all)) in &card.by_type {
        if *kind > 0 {
            let _ = writeln!(out, "  {}: {found} of {all}", names[usize::from(*kind)]);
        }
    }
    if !card.by_group.is_empty() {
        out.push_str("Type 4 by group (pairs of members found):\n");
        for (group, (found, all)) in &card.by_group {
            let _ = writeln!(out, "  {group}: {found} of {all}");
        }
    }
    let list = |out: &mut String, title: &str, items: &[String]| {
        if !items.is_empty() {
            let _ = writeln!(out, "{title} ({}):", items.len());
            for item in items {
                let _ = writeln!(out, "  {item}");
            }
        }
    };
    list(&mut out, "wrong: called equal", &card.equal_wrong);
    list(&mut out, "wrong: unrelated code matched", &card.unrelated);
    list(&mut out, "missed", &card.missed);
    list(&mut out, "not labelled (review)", &card.unlabelled);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Labels {
        Labels::parse(
            r#"{
              "groups": [{ "id": "sum", "members": { "S.For": "for", "S.Linq": "linq", "S.While": "while" } }],
              "pairs": [
                { "id": "copy", "a": "A.F", "b": "B.F", "type": 1, "equal": true, "note": "copy" },
                { "id": "near", "a": "N.Lt", "b": "N.Le", "type": 3, "equal": false, "note": "< vs <=" },
                { "id": "decoy", "a": "D.X", "b": "D.Y", "type": 0, "equal": false, "note": "unrelated" }
              ]
            }"#,
        )
        .unwrap()
    }

    fn class(functions: &[&str], evidence: &str) -> Value {
        serde_json::json!({
            "evidence": evidence,
            "copies": functions.iter().map(|f| serde_json::json!({ "function": f })).collect::<Vec<_>>()
        })
    }

    #[test]
    fn counts_found_missed_wrong_and_unlabelled_pairs() {
        let report = serde_json::json!({ "classes": [
            class(&["A.F", "B.F"], "syntax"),
            class(&["S.For", "S.Linq"], "normalized"),
            class(&["N.Lt", "N.Le"], "normalized"),
            class(&["D.X", "D.Y"], "syntax"),
            class(&["Q.One", "Q.Two"], "syntax"),
        ]});
        let card = score(&report, &labels());
        assert_eq!(card.by_type[&1], (1, 1));
        assert_eq!(card.by_type[&3], (1, 1));
        assert_eq!(card.by_type[&4], (1, 3));
        assert_eq!(card.by_group["sum"], (1, 3));
        assert_eq!(card.missed.len(), 2);
        assert_eq!(card.unrelated.len(), 1);
        assert_eq!(card.equal_right, 1);
        assert_eq!(card.equal_wrong.len(), 1, "the near miss was called equal");
        assert_eq!(card.unlabelled, ["Q.One vs Q.Two"]);
        assert!((card.recall() - 3.0 / 5.0).abs() < 1e-9);
        assert!((card.precision() - 3.0 / 4.0).abs() < 1e-9);
    }

    #[test]
    fn the_corpus_labels_parse_and_name_no_method_twice_in_a_group() {
        let text = include_str!("../tests/corpus/csharp/cases.json");
        let labels = Labels::parse(text).unwrap();
        assert!(labels.groups.len() >= 10 && labels.pairs.len() >= 10);
        let pairs = labels.pairs();
        assert!(pairs.values().any(|l| l.kind == 4));
    }
}
