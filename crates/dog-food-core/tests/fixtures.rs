//! Runs every file under `tests/fixtures/` and checks each marker comment
//! against the function that starts on the following line:
//!
//!     // dgf: 7 cognitive=9 nesting=3 params=2
//!
//! The first number is the cyclomatic complexity; the named values are optional.

use std::fs;
use std::path::{Path, PathBuf};

use dog_food_core::FunctionMetrics;

const MARKER: &str = "dgf:";

struct Expectation {
    line: usize,
    values: Vec<(String, u32)>,
}

#[test]
fn fixture_files_match_their_expectations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let files = fixture_files(&root);
    assert!(!files.is_empty(), "no fixtures found in {}", root.display());

    let mut failures = Vec::new();
    let mut checked = 0;
    for path in &files {
        let report = dog_food_core::analyze_file(path).unwrap();
        assert!(!report.has_errors, "{} has syntax errors", path.display());
        for expectation in expectations(path) {
            checked += 1;
            let found = report
                .functions
                .iter()
                .find(|f| f.start_line == expectation.line);
            failures.extend(check(path, &expectation, found));
        }
    }

    assert!(checked > 0, "fixtures contain no `{MARKER}` expectations");
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

fn check(path: &Path, expectation: &Expectation, found: Option<&FunctionMetrics>) -> Vec<String> {
    let at = format!("{}:{}", path.display(), expectation.line);
    let Some(f) = found else {
        return vec![format!("{at}: no function starts here")];
    };
    expectation
        .values
        .iter()
        .filter_map(|(key, expected)| {
            let actual = value(f, key);
            (actual != Some(*expected)).then(|| {
                format!(
                    "{at} {}: {key} expected {expected}, got {actual:?} {:?}",
                    f.name, f.breakdown
                )
            })
        })
        .collect()
}

fn value(f: &FunctionMetrics, key: &str) -> Option<u32> {
    match key {
        "complexity" => Some(f.complexity),
        "cognitive" => Some(f.cognitive),
        "nesting" => Some(f.max_nesting),
        "params" => Some(f.parameters),
        _ => None,
    }
}

fn expectations(path: &Path) -> Vec<Expectation> {
    let text = fs::read_to_string(path).unwrap();
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let (_, rest) = line.split_once(MARKER)?;
            let mut words = rest.split_whitespace();
            let complexity = words.next()?.parse().ok()?;
            let mut values = vec![("complexity".to_string(), complexity)];
            values.extend(words.filter_map(|word| {
                let (key, number) = word.split_once('=')?;
                Some((key.to_string(), number.parse().ok()?))
            }));
            Some(Expectation {
                line: index + 2,
                values,
            })
        })
        .collect()
}

fn fixture_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(fixture_files(&path));
        } else if dog_food_core::Language::from_path(&path).is_some() {
            files.push(path);
        }
    }
    files
}
