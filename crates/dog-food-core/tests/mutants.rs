//! Fixture files in `tests/mutants/` declare the mutants each line must get:
//!
//! ```text
//! // mutant: relational > -> >=
//! return a > 1;
//! ```
//!
//! A marker belongs to the next line that is not a comment or blank; every
//! mutant on a line needs a marker and every marker a mutant. The original
//! is written with whitespace runs as one space, and may end in `...` to
//! match only its start. `(boundary-risk)` expects a date comparison, and
//! `(ignored)` at the end the mutant turned off.
//! A first line `// settings: level=advanced ignore_methods=*Log*,ToString`
//! sets what is mutated.

use std::collections::BTreeMap;
use std::path::Path;

use dog_food_core::mutate::{mutate, schemata, Level, Settings};
use dog_food_core::{Language, Mutant};

#[test]
fn every_fixture_gets_exactly_its_declared_mutants() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mutants");
    let mut failures = Vec::new();
    let mut checked = 0;
    for language in std::fs::read_dir(&root).unwrap() {
        for file in std::fs::read_dir(language.unwrap().path()).unwrap() {
            let path = file.unwrap().path();
            failures.extend(check(&path));
            checked += 1;
        }
    }
    assert!(checked >= 2, "no fixtures found in {}", root.display());
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

fn check(path: &Path) -> Vec<String> {
    let source = std::fs::read_to_string(path).unwrap();
    let language = Language::from_path(path).unwrap();
    let settings = settings(&source);
    let found = mutate(&source, language, &settings)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    let mut failures = switched_parses(path, &source, language, &found.mutants);
    let expected = expected(&source);
    let mut actual: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for m in &found.mutants {
        actual.entry(m.line).or_default().push(marker(m));
    }
    let lines: Vec<usize> = expected.keys().chain(actual.keys()).copied().collect();
    for line in lines.into_iter().collect::<std::collections::BTreeSet<_>>() {
        let mut want = expected.get(&line).cloned().unwrap_or_default();
        let mut got = actual.get(&line).cloned().unwrap_or_default();
        want.sort();
        got.sort();
        let matches = want.len() == got.len() && want.iter().zip(&got).all(|(w, g)| same(w, g));
        if !matches {
            failures.push(format!(
                "{}:{line}\n  expected: {want:#?}\n  got:      {got:#?}",
                path.display()
            ));
        }
    }
    failures
}

/// The file with every active mutant switched in must still parse.
fn switched_parses(
    path: &Path,
    source: &str,
    language: Language,
    mutants: &[Mutant],
) -> Vec<String> {
    let active: Vec<&Mutant> = mutants.iter().filter(|m| m.ignored.is_none()).collect();
    let switched = schemata(source, language, &active, &|id| format!("isOn({id})"));
    if reported_lines(&switched) != source.lines().count() {
        return vec![format!(
            "{}: switching moved lines:\n{switched}",
            path.display()
        )];
    }
    // tree-sitter-c-sharp cannot parse the empty interpolated string `$""`,
    // which C# compiles.
    if has_errors(&switched.replace("$\"\"", "\"\""), language) {
        return vec![format!(
            "{}: the switched file does not parse:
{switched}",
            path.display()
        )];
    }
    Vec::new()
}

/// The number the compiler gives the last line: one per line, but a
/// `#line N` directive is no line, and the line after it is `N`.
fn reported_lines(text: &str) -> usize {
    let mut number = 0;
    for line in text.lines() {
        let directive = line.trim().strip_prefix("#line ");
        match directive.and_then(|n| n.trim().parse::<usize>().ok()) {
            Some(next) => number = next - 1,
            None => number += 1,
        }
    }
    number
}

fn has_errors(source: &str, language: Language) -> bool {
    let dir = tempfile_dir();
    let path = dir.join(format!("switched.{}", extension(language)));
    std::fs::write(&path, source).unwrap();
    dog_food_core::analyze_file(&path).unwrap().has_errors
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dgf-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn extension(language: Language) -> &'static str {
    match language {
        Language::CSharp => "cs",
        Language::Rust => "rs",
        Language::Tsx => "tsx",
        _ => "ts",
    }
}

/// A mutant written as its marker would be.
fn marker(m: &Mutant) -> String {
    let ignored = if m.ignored.is_some() {
        " (ignored)"
    } else {
        ""
    };
    let boundary = if m.boundary_risk {
        " (boundary-risk)"
    } else {
        ""
    };
    format!(
        "{} {} -> {}{boundary}{ignored}",
        m.mutator,
        collapse(&m.original),
        m.replacement
    )
}

fn settings(source: &str) -> Settings {
    let mut settings = Settings::default();
    let Some(line) = source
        .lines()
        .next()
        .and_then(|l| l.trim().strip_prefix("// settings:"))
    else {
        return settings;
    };
    for part in line.split_whitespace() {
        match part.split_once('=') {
            Some(("level", level)) => settings.level = level.parse::<Level>().unwrap(),
            Some(("ignore_methods", list)) => {
                settings.ignore_methods = list.split(',').map(str::to_string).collect();
            }
            _ => panic!("unknown setting {part}"),
        }
    }
    settings
}

/// Markers by the line they belong to.
fn expected(source: &str) -> BTreeMap<usize, Vec<String>> {
    let mut expected: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut pending = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(marker) = trimmed.strip_prefix("// mutant:") {
            pending.push(marker.trim().to_string());
        } else if !trimmed.is_empty() && !trimmed.starts_with("//") && !pending.is_empty() {
            expected.entry(index + 1).or_default().append(&mut pending);
        }
    }
    assert!(pending.is_empty(), "markers at the end of the file");
    expected
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `want` may shorten the original with `...`.
fn same(want: &str, got: &str) -> bool {
    if want == got {
        return true;
    }
    let (Some((w_head, w_tail)), Some((g_head, g_tail))) =
        (want.rsplit_once(" -> "), got.rsplit_once(" -> "))
    else {
        return false;
    };
    w_tail == g_tail
        && w_head
            .strip_suffix("...")
            .is_some_and(|prefix| g_head.starts_with(prefix))
}
