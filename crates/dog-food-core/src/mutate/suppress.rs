//! `dgf-disable` comments: mutants a person decided not to test, with the
//! reason written next to them.
//!
//! ```text
//! // dgf-disable-once relational, string: reason   the next statement, or the
//!                                                  statement this comment ends
//! // dgf-disable-next-line all: reason             mutants on the next line
//! // dgf-disable all: reason                       from here ...
//! // dgf-restore all                               ... to here (or the file's end)
//! ```
//!
//! Mutator names are the registry's (`relational`, `string`), in any
//! case, or `all`. A disable without a reason is an error, so every suppression says
//! why.

use serde::Serialize;
use tree_sitter::{Node, Tree};

use super::{is_known, names, walk, Mutant};
use crate::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suppression {
    /// 1-based line of the comment.
    pub line: usize,
    /// `once`, `next-line` or `range`.
    pub kind: &'static str,
    pub mutators: Vec<String>,
    pub reason: String,
    /// The last line it covers.
    pub end_line: usize,
    /// How many mutants it turned off.
    pub mutants: usize,
    /// How many of them move a date or time boundary.
    #[serde(skip_serializing_if = "is_zero")]
    pub boundary: usize,
    /// Tests the reason names (`test: Class.Method`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The test names after `test:` in a reason: `test: PlanTests.EndsOnLastDay`.
fn named_tests(reason: &str) -> Vec<String> {
    let lower = reason.to_ascii_lowercase();
    let Some(at) = lower.find("test:") else {
        return Vec::new();
    };
    reason[at + 5..]
        .split([',', ' ', ';'])
        .map(|t| t.trim_end_matches(['.', ')']))
        .filter(|t| {
            !t.is_empty()
                && t.chars()
                    .all(|c| c.is_alphanumeric() || c == '.' || c == '_')
        })
        .map(str::to_string)
        .collect()
}

/// One parsed comment.
struct Directive {
    kind: &'static str,
    names: Vec<String>,
    reason: String,
}

/// Reads every `dgf-disable` comment, marks the mutants they cover as
/// ignored and returns the comments.
pub fn apply(tree: &Tree, source: &str, mutants: &mut [Mutant]) -> Result<Vec<Suppression>, Error> {
    let mut comments = Vec::new();
    walk(tree.root_node(), &mut |node| {
        if node.kind().contains("comment") {
            comments.push(node);
        }
    });
    let mut found: Vec<Covered> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for comment in &comments {
        let line = comment.start_position().row + 1;
        let Some(directive) = parse(super::text(*comment, source.as_bytes()), line)? else {
            continue;
        };
        if directive.kind == "restore" {
            close(&mut found, &mut open, &directive.names, comment, source);
            continue;
        }
        if directive.kind == "range" {
            open.push(found.len());
        }
        let place = Place {
            root: tree.root_node(),
            comment: *comment,
            comments: &comments,
            source,
        };
        found.push(covered(directive, &place));
    }
    mark(mutants, &mut found)?;
    Ok(found.into_iter().map(|(s, _, _)| s).collect())
}

/// A suppression and the bytes it covers.
type Covered = (Suppression, usize, usize);

/// Where a comment is.
struct Place<'a, 't> {
    root: Node<'t>,
    comment: Node<'t>,
    comments: &'a [Node<'t>],
    source: &'a str,
}

fn covered(directive: Directive, place: &Place) -> Covered {
    let src = place.source.as_bytes();
    let (start, end) = match directive.kind {
        "once" => once(place.root, place.comment, place.comments, src),
        "next-line" => next_line(&place.comment, place.source),
        _ => (place.comment.end_byte(), place.source.len()),
    };
    let tests = named_tests(&directive.reason);
    let suppression = Suppression {
        line: place.comment.start_position().row + 1,
        kind: directive.kind,
        mutators: directive.names,
        reason: directive.reason,
        end_line: super::position(src, end.max(start)).0,
        mutants: 0,
        boundary: 0,
        tests,
    };
    (suppression, start, end)
}

/// Marks every mutant a suppression covers as ignored, with its reason. A
/// date boundary may only be turned off by a reason that names a test.
fn mark(mutants: &mut [Mutant], found: &mut [Covered]) -> Result<(), Error> {
    for mutant in mutants.iter_mut().filter(|m| m.ignored.is_none()) {
        let covering = found
            .iter_mut()
            .find(|(s, start, end)| covers(s, *start, *end, mutant));
        let Some((suppression, _, _)) = covering else {
            continue;
        };
        if mutant.boundary_risk && suppression.tests.is_empty() {
            return Err(Error::Suppression {
                line: suppression.line,
                message: format!(
                    "this comment turns off a date boundary mutant (line {}: {} -> {}); name the test that pins the boundary in the reason, for example `: the last day is inclusive, test: PlanTests.EndsOnLastDay`",
                    mutant.line, mutant.original, mutant.replacement
                ),
            });
        }
        suppression.mutants += 1;
        suppression.boundary += usize::from(mutant.boundary_risk);
        mutant.ignored = Some(format!(
            "{} (dgf-disable, line {})",
            suppression.reason, suppression.line
        ));
    }
    Ok(())
}

fn covers(suppression: &Suppression, start: usize, end: usize, mutant: &Mutant) -> bool {
    start <= mutant.start_byte
        && mutant.end_byte <= end
        && suppression
            .mutators
            .iter()
            .any(|n| names(n, mutant.mutator))
}

/// The directive in a comment, if it is one.
fn parse(comment: &str, line: usize) -> Result<Option<Directive>, Error> {
    let Some((kind, rest)) = directive(comment) else {
        return Ok(None);
    };
    let (list, reason) = rest.split_once(':').unwrap_or((rest, ""));
    let mut names = mutator_names(list, kind, line)?;
    if kind == "restore" {
        if names.is_empty() {
            names.push("all".into());
        }
        return Ok(Some(Directive {
            kind,
            names,
            reason: String::new(),
        }));
    }
    let reason = reason.trim();
    check_disable(kind, &names, reason, line)?;
    Ok(Some(Directive {
        kind,
        names,
        reason: reason.to_string(),
    }))
}

/// Comment words and the kinds they make; longer words first.
const DIRECTIVES: &[(&str, &str)] = &[
    ("disable-once", "once"),
    ("disable-next-line", "next-line"),
    ("disable", "range"),
    ("restore", "restore"),
];

/// The kind of a `dgf-...` comment and the text after its word.
fn directive(comment: &str) -> Option<(&'static str, &str)> {
    let body = comment
        .trim_start_matches('/')
        .trim_start_matches('*')
        .trim_end_matches('/')
        .trim_end_matches('*')
        .trim();
    let rest = body.strip_prefix("dgf-")?;
    let (word, kind) = DIRECTIVES.iter().find(|(word, _)| rest.starts_with(word))?;
    let rest = &rest[word.len()..];
    (rest.is_empty() || rest.starts_with([' ', '\t', ':'])).then_some((*kind, rest))
}

fn mutator_names(list: &str, kind: &str, line: usize) -> Result<Vec<String>, Error> {
    let names: Vec<String> = list
        .split([',', ' ', '\t'])
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect();
    match names.iter().find(|n| !is_known(n)) {
        Some(unknown) => Err(Error::Suppression {
            line,
            message: format!(
                "dgf-{} names an unknown mutator {unknown:?}",
                directive_name(kind)
            ),
        }),
        None => Ok(names),
    }
}

/// A disable needs the mutators it turns off and a reason.
fn check_disable(kind: &str, names: &[String], reason: &str, line: usize) -> Result<(), Error> {
    let word = directive_name(kind);
    let message = if names.is_empty() {
        format!("dgf-{word} needs the mutators it turns off, or all")
    } else if reason.is_empty() {
        format!(
            "dgf-{word} needs a reason after ':', for example `// dgf-{word} {}: why the tests need not notice this`",
            names.join(", ")
        )
    } else {
        return Ok(());
    };
    Err(Error::Suppression { line, message })
}

fn directive_name(kind: &str) -> &'static str {
    DIRECTIVES
        .iter()
        .find(|(_, k)| *k == kind)
        .map_or("disable", |(word, _)| word)
}

/// Ends the open ranges that `restored` names: a range for other mutators
/// as well goes on for them.
fn close(
    found: &mut Vec<Covered>,
    open: &mut Vec<usize>,
    restored: &[String],
    comment: &Node,
    source: &str,
) {
    let at = comment.start_byte();
    let line = comment.start_position().row + 1;
    let all = restored.iter().any(|n| n.eq_ignore_ascii_case("all"));
    let mut still_open = Vec::new();
    for index in std::mem::take(open) {
        let (suppression, _, end) = &mut found[index];
        let remaining: Vec<String> = suppression
            .mutators
            .iter()
            .filter(|n| !all && !restored.iter().any(|r| r.eq_ignore_ascii_case(n)))
            .cloned()
            .collect();
        if remaining.len() == suppression.mutators.len() {
            still_open.push(index);
            continue;
        }
        *end = at;
        suppression.end_line = line;
        if !remaining.is_empty() {
            let rest = Suppression {
                mutators: remaining,
                end_line: super::position(source.as_bytes(), source.len()).0,
                ..suppression.clone()
            };
            still_open.push(found.len());
            found.push((rest, comment.end_byte(), source.len()));
        }
    }
    *open = still_open;
}

/// The bytes `dgf-disable-once` covers: the statement the comment ends, or
/// else the next one.
fn once(root: Node, comment: Node, comments: &[Node], src: &[u8]) -> (usize, usize) {
    let line_start = src[..comment.start_byte()]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    let before = &src[line_start..comment.start_byte()];
    if let Some(last) = before.iter().rposition(|b| !b.is_ascii_whitespace()) {
        let at = line_start + last;
        let mut node = root.descendant_for_byte_range(at, at + 1).unwrap_or(root);
        while let Some(parent) = node.parent() {
            if parent.end_byte() > comment.start_byte() {
                break;
            }
            node = parent;
        }
        return (node.start_byte(), node.end_byte());
    }
    let Some(at) = next_code(comment.end_byte(), comments, src) else {
        return (0, 0);
    };
    let mut node = root.descendant_for_byte_range(at, at + 1).unwrap_or(root);
    while let Some(parent) = node.parent() {
        if parent.start_byte() != at {
            break;
        }
        node = parent;
    }
    (node.start_byte(), node.end_byte())
}

/// The first byte of code after `from`, past whitespace and comments.
fn next_code(from: usize, comments: &[Node], src: &[u8]) -> Option<usize> {
    let mut at = from;
    loop {
        while at < src.len() && src[at].is_ascii_whitespace() {
            at += 1;
        }
        match comments.iter().find(|c| c.start_byte() == at) {
            Some(comment) => at = comment.end_byte(),
            None => return (at < src.len()).then_some(at),
        }
    }
}

/// The bytes of the line after the comment.
fn next_line(comment: &Node, source: &str) -> (usize, usize) {
    let src = source.as_bytes();
    let Some(newline) = src[comment.end_byte()..].iter().position(|&b| b == b'\n') else {
        return (0, 0);
    };
    let start = comment.end_byte() + newline + 1;
    let end = src[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(src.len(), |p| start + p);
    (start, end)
}

#[cfg(test)]
mod tests {
    use crate::mutate::{mutate, Settings};
    use crate::{Error, Language};

    fn ignored(source: &str) -> Vec<(String, Option<String>)> {
        mutate(source, Language::CSharp, &Settings::default())
            .unwrap()
            .mutants
            .into_iter()
            .map(|m| (m.replacement, m.ignored))
            .collect()
    }

    #[test]
    fn once_covers_the_next_statement_or_the_one_it_ends() {
        let src = "class C { int M(int a) {
  // dgf-disable-once relational: only the sign matters
  var b = a > 1;
  var c = a < 2; // dgf-disable-once Relational: same
  return a * 2;
} }";
        let found = ignored(src);
        let reason = |r: &str| found.iter().find(|(x, _)| x == r).unwrap().1.clone();
        assert_eq!(
            reason("<").as_deref(),
            Some("only the sign matters (dgf-disable, line 2)")
        );
        assert_eq!(reason(">").as_deref(), Some("same (dgf-disable, line 4)"));
        assert_eq!(reason("/"), None);
    }

    #[test]
    fn ranges_and_next_line() {
        let src = "class C { int M(int a) {
  // dgf-disable all: generated mapping
  var b = a > 1;
  // dgf-restore all
  // dgf-disable-next-line arithmetic: units only
  return a * 2 - (a == 3 ? 1 : 0);
} }";
        let result = mutate(src, Language::CSharp, &Settings::default()).unwrap();
        let by = |r: &str| {
            result
                .mutants
                .iter()
                .find(|m| m.replacement == r)
                .unwrap()
                .ignored
                .clone()
        };
        assert!(by("<").is_some());
        assert!(by("/").is_some() && by("+").is_some());
        assert!(by("!=").is_none());
        assert_eq!(result.suppressions.len(), 2);
        assert_eq!(result.suppressions[0].end_line, 4);
        assert_eq!(result.suppressions[0].mutants, 2);
        assert_eq!(result.suppressions[1].kind, "next-line");
    }

    #[test]
    fn a_date_boundary_needs_a_named_test() {
        let bare = "class C { bool M(System.DateOnly end, System.DateOnly today) {\n  // dgf-disable-once relational: inclusive\n  return today <= end; } }";
        let err = mutate(bare, Language::CSharp, &Settings::default()).unwrap_err();
        assert!(
            matches!(&err, Error::Suppression { line: 2, message } if message.contains("name the test")),
            "{err}"
        );
        let named = "class C { bool M(System.DateOnly end, System.DateOnly today) {\n  // dgf-disable-once relational: inclusive, test: PlanTests.EndsOnLastDay\n  return today <= end; } }";
        let found = mutate(named, Language::CSharp, &Settings::default()).unwrap();
        assert_eq!(found.suppressions[0].tests, ["PlanTests.EndsOnLastDay"]);
        assert_eq!(found.suppressions[0].boundary, 2);
    }

    #[test]
    fn a_disable_needs_a_reason_and_known_names() {
        let missing = "class C { int M(int a) {\n  // dgf-disable-once relational\n  return a > 1 ? 1 : 0; } }";
        let err = mutate(missing, Language::CSharp, &Settings::default()).unwrap_err();
        assert!(
            matches!(&err, Error::Suppression { line: 2, message } if message.contains("needs a reason")),
            "{err}"
        );
        let unknown =
            "class C { int M(int a) {\n  // dgf-disable-once relationl: typo\n  return a; } }";
        assert!(mutate(unknown, Language::CSharp, &Settings::default()).is_err());
        let prose = "class C { int M(int a) {\n  // dgf-disabled by default\n  return a; } }";
        assert!(mutate(prose, Language::CSharp, &Settings::default()).is_ok());
    }
}
