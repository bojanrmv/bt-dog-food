//! `dgf mutate`: chooses the functions worth mutating, lists the mutants in
//! them, and reports what the engine (`crate::mutation`) found when they ran.
//! See docs/mutation-testing.md.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};

use dog_food_core::mutate::{Settings, Suppression};
use dog_food_core::{FileReport, Language, Mutant};
use rayon::prelude::*;
use serde::Serialize;

use crate::changes::{Change, ChangesRun};
use crate::config::Thresholds;
use crate::mutation::{Outcome, RunResult, Untested};
use crate::report::{now, Format};

/// One function chosen for mutation, with its mutants.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub file: String,
    /// Where the file is, as found (not shown).
    #[serde(skip)]
    pub path: PathBuf,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub complexity: u32,
    /// Why it was chosen: `new`, `worse`, `better`, `same` (from `--changes`),
    /// `tests` (a test that reaches it changed) or `selected`.
    pub reason: &'static str,
    pub mutants: Vec<Mutant>,
    /// `dgf-disable` comments that cover part of the function.
    pub suppressions: Vec<Suppression>,
}

/// A function's place in a file, before its mutants are known.
pub struct Wanted {
    pub file: String,
    pub path: PathBuf,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub complexity: u32,
    pub reason: &'static str,
    /// With `--scope lines`, the changed lines of the file: only mutants
    /// there are kept.
    pub lines: Option<Vec<(usize, usize)>>,
}

/// Which mutants of a changed function `--changes` keeps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Scope {
    /// Every mutant of a function with a changed line.
    #[default]
    Functions,
    /// Only mutants on changed lines.
    Lines,
}

/// Why a function is chosen when it is only next to the change: it holds a
/// string key a changed function also holds. Its mutants are listed apart
/// and left out of the score.
pub const NEIGHBOUR: &str = "neighbour";

/// The string keys of a change: quoted literals of three characters or more
/// with a letter and only letters, digits and `_ . - :` (such as
/// `"MemberPlanId"`), and the names of constants that hold one.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Keys {
    pub literals: BTreeSet<String>,
    pub names: BTreeSet<String>,
    /// The languages of the change: keys are not looked for in others.
    languages: Vec<Language>,
}

impl Keys {
    /// The keys of `targets` (test code left out): the literals in the
    /// functions, and the constants of their files that the functions use.
    pub fn of(targets: &[Target]) -> Keys {
        let mut keys = Keys::default();
        let mut constants: BTreeMap<&PathBuf, Vec<(String, Vec<String>)>> = BTreeMap::new();
        for t in targets
            .iter()
            .filter(|t| t.reason != NEIGHBOUR && !is_test_code(&t.path))
        {
            if let Some(language) = Language::from_path(&t.path) {
                if !keys.languages.contains(&language) {
                    keys.languages.push(language);
                }
            }
            let file = constants
                .entry(&t.path)
                .or_insert_with(|| file_constants(&t.path));
            let text = std::fs::read_to_string(&t.path).unwrap_or_default();
            let body: Vec<&str> = text
                .lines()
                .skip(t.start_line.saturating_sub(1))
                .take(t.end_line + 1 - t.start_line)
                .collect();
            for line in &body {
                keys.literals.extend(literals(line));
            }
            for (name, held) in file.iter() {
                if body.iter().any(|line| has_word(line, name)) {
                    keys.names.insert(name.clone());
                    keys.literals.extend(held.iter().cloned());
                }
            }
        }
        keys
    }

    pub fn is_empty(&self) -> bool {
        self.literals.is_empty()
    }

    pub fn len(&self) -> usize {
        self.literals.len()
    }

    /// Whether `line` holds a key literal or names a constant that holds one.
    fn in_line(&self, line: &str) -> bool {
        self.literals.iter().any(|k| line.contains(k.as_str()))
            || self.names.iter().any(|n| has_word(line, n))
    }
}

/// The constants a file declares with a key: (name, its literals).
fn file_constants(path: &Path) -> Vec<(String, Vec<String>)> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let held = literals(line);
            let name = constant_name(line).filter(|_| !held.is_empty())?;
            Some((name.to_string(), held))
        })
        .collect()
}

/// The quoted keys on a line, quotes included.
fn literals(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    for quote in ['"', '\''] {
        let parts: Vec<&str> = line.split(quote).collect();
        // Odd parts sit between a pair of quotes.
        for inner in parts.iter().skip(1).step_by(2).take(parts.len() / 2) {
            let is_key = inner.len() >= 3
                && inner.chars().any(char::is_alphabetic)
                && inner
                    .chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | ':'));
            if is_key {
                found.push(format!("{quote}{inner}{quote}"));
            }
        }
    }
    found
}

/// `X` in `const string X = "..."` (C#) or `const X = '...'` (TypeScript).
fn constant_name(line: &str) -> Option<&str> {
    let (before, _) = line.split_once('=')?;
    let before = before.trim_end();
    let declares = before
        .split_whitespace()
        .any(|w| w == "const" || w == "readonly");
    let name = before
        .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
        .next()?;
    (declares && !name.is_empty()).then_some(name)
}

/// Whether `word` appears in `line` with no identifier character around it.
fn has_word(line: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(word).any(|(at, _)| {
        let before = line[..at].chars().next_back().is_none_or(|c| !ident(c));
        let after = line[at + word.len()..]
            .chars()
            .next()
            .is_none_or(|c| !ident(c));
        before && after
    })
}

/// Functions in `files` (absolute paths) that hold one of `keys`, other
/// than those in `taken` (file, first line), with only the lines that hold
/// a key: the code that writes or reads the same keys as the change.
pub fn neighbours(
    files: &[PathBuf],
    root: &Path,
    keys: &Keys,
    taken: &HashSet<(String, usize)>,
) -> Vec<Wanted> {
    if keys.is_empty() {
        return Vec::new();
    }
    files
        .par_iter()
        .filter(|path| !is_test_code(path))
        .flat_map_iter(|path| {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let found = keys.literals.iter().any(|k| text.contains(k.as_str()))
                || keys.names.iter().any(|n| text.contains(n.as_str()));
            let functions = Language::from_path(path)
                .filter(|language| found && keys.languages.contains(language))
                .and_then(|language| dog_food_core::analyze_source(&text, language).ok())
                .unwrap_or_default();
            let file = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let lines: Vec<&str> = text.lines().collect();
            functions
                .into_iter()
                .filter(|f| !taken.contains(&(file.clone(), f.start_line)))
                .filter_map(|f| {
                    let holding: Vec<(usize, usize)> = (f.start_line..=f.end_line)
                        .filter(|n| lines.get(n - 1).is_some_and(|l| keys.in_line(l)))
                        .map(|n| (n, n))
                        .collect();
                    (!holding.is_empty()).then(|| Wanted {
                        file: file.clone(),
                        path: path.clone(),
                        name: f.name,
                        start_line: f.start_line,
                        end_line: f.end_line,
                        complexity: f.complexity,
                        reason: NEIGHBOUR,
                        lines: Some(holding),
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Why a plain constructor's mutants are not tested.
const PLAIN_CONSTRUCTOR: &str =
    "a constructor that only stores its arguments ([mutate] skip_plain_constructors)";

/// Marks the mutants of C# constructors that only store their arguments
/// (`_repo = repo;`, `this.log = log ?? throw ...;`) as ignored: every test
/// that uses the object fails without them, so they cost a run and say
/// nothing.
pub fn skip_plain_constructors(targets: &mut [Target]) {
    for target in targets.iter_mut() {
        let text = std::fs::read_to_string(&target.path).unwrap_or_default();
        let body: Vec<&str> = text
            .lines()
            .skip(target.start_line.saturating_sub(1))
            .take(target.end_line + 1 - target.start_line)
            .collect();
        if !is_constructor(&target.name) || !only_stores(&body) {
            continue;
        }
        for mutant in target.mutants.iter_mut().filter(|m| m.ignored.is_none()) {
            mutant.ignored = Some(PLAIN_CONSTRUCTOR.into());
        }
    }
}

/// `Type.Type`: how C# constructors are named.
fn is_constructor(name: &str) -> bool {
    let mut parts = name.rsplit('.');
    matches!((parts.next(), parts.next()), (Some(a), Some(b)) if a == b)
}

/// Whether every statement in a constructor's body, after its `{`, stores a
/// name in a field: `x = y;`, `this.x = y;`, `x = y ?? throw ...;`.
fn only_stores(lines: &[&str]) -> bool {
    let Some(open) = lines.iter().position(|l| l.contains('{')) else {
        return false;
    };
    let after = lines[open].split_once('{').map_or("", |(_, rest)| rest);
    let statements: Vec<&str> = std::iter::once(after)
        .chain(lines[open + 1..].iter().copied())
        .map(|l| l.trim().trim_end_matches('}').trim())
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect();
    let name = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_');
    let stores = |line: &str| {
        let Some((field, value)) = line.trim_end_matches(';').split_once(" = ") else {
            return false;
        };
        let field = field.trim().trim_start_matches("this.");
        let value = value.split(" ?? throw ").next().unwrap_or("").trim();
        line.ends_with(';') && name(field) && name(value)
    };
    !statements.is_empty() && statements.iter().all(|l| stores(l))
}

/// The targets a score and exit code are about: not the neighbours.
fn gated(targets: &[Target]) -> impl Iterator<Item = &Mutant> {
    targets
        .iter()
        .filter(|t| t.reason != NEIGHBOUR)
        .flat_map(|t| &t.mutants)
}

/// Every function in `reports` with at least `min_complexity`.
pub fn from_reports(
    reports: &[FileReport],
    min_complexity: u32,
    display: impl Fn(&str) -> String,
) -> Vec<Wanted> {
    reports
        .iter()
        .flat_map(|report| {
            let file = display(&report.path);
            report
                .functions
                .iter()
                .filter(move |f| f.complexity >= min_complexity && f.ignored.is_none())
                .map(move |f| Wanted {
                    file: file.clone(),
                    path: PathBuf::from(&report.path),
                    name: f.name.clone(),
                    start_line: f.start_line,
                    end_line: f.end_line,
                    complexity: f.complexity,
                    reason: "selected",
                    lines: None,
                })
        })
        .collect()
}

/// Functions that `dgf changes` found new or touched, at least `min_complexity`.
pub fn from_changes(run: &ChangesRun, root: &Path, min_complexity: u32) -> Vec<Wanted> {
    run.rows
        .iter()
        .filter(|row| row.change != Change::Removed && row.after.unwrap_or(0) >= min_complexity)
        .map(|row| Wanted {
            file: row.file.clone(),
            path: root.join(&row.file),
            name: row.name.clone(),
            start_line: row.start_line,
            end_line: row.end_line,
            complexity: row.after.unwrap_or(0),
            reason: change_reason(row.change),
            lines: None,
        })
        .collect()
}

/// The function `name` (a stable name, without positions) in `file`, chosen
/// because a test that reaches it changed.
pub fn function_named(root: &Path, file: &str, name: &str) -> Vec<Wanted> {
    let path = root.join(file);
    let Ok(report) = dog_food_core::analyze_file(&path) else {
        return Vec::new();
    };
    report
        .functions
        .iter()
        .filter(|f| crate::changes::stable_name(&f.name) == name)
        .map(|f| Wanted {
            file: file.to_string(),
            path: path.clone(),
            name: f.name.clone(),
            start_line: f.start_line,
            end_line: f.end_line,
            complexity: f.complexity,
            reason: "tests",
            lines: None,
        })
        .collect()
}

fn change_reason(change: Change) -> &'static str {
    match change {
        Change::New => "new",
        Change::Worse => "worse",
        Change::Better => "better",
        _ => "same",
    }
}

/// Tests are what judges the mutants, so they are never mutated themselves:
/// `*.test.ts`, `*.spec.ts`, `*Tests.cs`, and anything in a `test`, `tests`,
/// `__tests__` or `*.Tests` folder.
pub fn is_test_code(path: &Path) -> bool {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = stem.to_lowercase();
    // Case matters for `Test`: `OrderTests` is a test, `Latest` is not.
    let named = lower.ends_with(".test")
        || lower.ends_with(".spec")
        || stem.ends_with("Tests")
        || stem.ends_with("Test");
    let folder = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .any(|c| {
            let part = c.as_os_str().to_string_lossy().to_lowercase();
            matches!(part.as_str(), "test" | "tests" | "__tests__" | "spec")
                || part.ends_with(".tests")
                || part.ends_with(".test")
                || part.ends_with(".unittests")
                || part.ends_with(".integrationtests")
        });
    named || folder
}

/// A file that could not be mutated: skipped with a warning, or (a broken
/// `dgf-disable` comment, a bad setting) an error that stops the run.
enum Problem {
    Warning(String),
    Fatal(String),
}

/// Reads each file once, finds its mutants and gives every mutant to the
/// innermost wanted function that contains it. Errors: `dgf-disable`
/// comments without a reason, and bad settings.
pub fn targets(
    wanted: Vec<Wanted>,
    settings: &Settings,
) -> Result<(Vec<Target>, Vec<String>), String> {
    let mut by_file: BTreeMap<PathBuf, Vec<Wanted>> = BTreeMap::new();
    for w in wanted.into_iter().filter(|w| !is_test_code(&w.path)) {
        by_file.entry(w.path.clone()).or_default().push(w);
    }
    let results: Vec<Result<Vec<Target>, Problem>> = by_file
        .into_par_iter()
        .map(|(path, functions)| file_targets(&path, functions, settings))
        .collect();
    let mut targets = Vec::new();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(mut list) => targets.append(&mut list),
            Err(Problem::Warning(warning)) => warnings.push(warning),
            Err(Problem::Fatal(error)) => errors.push(error),
        }
    }
    if !errors.is_empty() {
        errors.sort();
        return Err(errors.join("\n"));
    }
    targets.sort_by(|a, b| {
        b.mutants
            .len()
            .cmp(&a.mutants.len())
            .then_with(|| a.file.cmp(&b.file))
    });
    Ok((targets, warnings))
}

fn file_targets(
    path: &Path,
    functions: Vec<Wanted>,
    settings: &Settings,
) -> Result<Vec<Target>, Problem> {
    let warn = |text: String| Problem::Warning(format!("{}: {text}", path.display()));
    let language = Language::from_path(path).ok_or_else(|| warn("unsupported file".into()))?;
    let source = std::fs::read_to_string(path).map_err(|err| warn(err.to_string()))?;
    let shown = functions
        .first()
        .map_or_else(|| path.display().to_string(), |w| w.file.clone());
    let found =
        dog_food_core::mutate::mutate(&source, language, settings).map_err(|err| match err {
            dog_food_core::Error::Suppression { line, message } => {
                Problem::Fatal(format!("{shown}:{line}: {message}"))
            }
            dog_food_core::Error::Settings(message) => Problem::Fatal(message),
            other => warn(other.to_string()),
        })?;
    let lines = functions.first().and_then(|w| w.lines.clone());
    let mut targets: Vec<Target> = functions
        .into_iter()
        .map(|w| Target {
            file: w.file,
            path: w.path,
            name: w.name,
            start_line: w.start_line,
            end_line: w.end_line,
            complexity: w.complexity,
            reason: w.reason,
            mutants: Vec::new(),
            suppressions: Vec::new(),
        })
        .collect();
    let on_lines = |m: &Mutant| {
        lines.as_ref().is_none_or(|ranges| {
            ranges
                .iter()
                .any(|&(from, to)| m.line <= to && from <= m.end_line)
        })
    };
    let bytes = byte_ranges(&targets, &source, language);
    for mutant in found.mutants.into_iter().filter(on_lines) {
        if let Some(index) = innermost(&targets, &bytes, &mutant) {
            targets[index].mutants.push(mutant);
        }
    }
    for target in &mut targets {
        target.suppressions = found
            .suppressions
            .iter()
            .filter(|s| s.line <= target.end_line && target.start_line <= s.end_line)
            .cloned()
            .collect();
    }
    Ok(targets)
}

/// Tests named by `dgf-disable` comments that excuse a date boundary must
/// exist: such a comment says a test pins the boundary.
pub fn check_named_tests(targets: &[Target], root: &Path) -> Result<(), String> {
    let named: Vec<(&str, usize, &str)> = targets
        .iter()
        .flat_map(|t| t.suppressions.iter().map(move |s| (t.file.as_str(), s)))
        .filter(|(_, s)| s.boundary > 0)
        .flat_map(|(file, s)| {
            s.tests
                .iter()
                .map(move |test| (file, s.line, test.as_str()))
        })
        .collect();
    if named.is_empty() {
        return Ok(());
    }
    let sources: Vec<String> = ignore::WalkBuilder::new(root)
        .build()
        .flatten()
        .map(|e| e.into_path())
        .filter(|p| p.is_file() && is_test_code(p) && Language::from_path(p).is_some())
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    let missing: Vec<String> = named
        .iter()
        .filter(|(_, _, test)| {
            let method = test.rsplit('.').next().unwrap_or(test);
            !sources.iter().any(|s| s.contains(&format!("{method}(")))
        })
        .map(|(file, line, test)| {
            format!("{file}:{line}: the test {test} named to excuse a date boundary was not found in the test code")
        })
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}

/// A mutant's original text on one line, shortened: a removed block can be
/// long.
pub fn short(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 60 {
        return flat;
    }
    let head: String = flat.chars().take(57).collect();
    format!("{head}...")
}

/// `  file:line  kind mutators: reason (N mutants)` for every suppression.
fn suppressions_text(targets: &[Target], indent: &str) -> String {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = String::new();
    for t in targets {
        for s in &t.suppressions {
            if seen.insert((t.file.clone(), s.line)) {
                let _ = writeln!(
                    out,
                    "{indent}{}:{}  dgf-disable{} {}: {} ({} mutants)",
                    t.file,
                    s.line,
                    match s.kind {
                        "once" => "-once",
                        "next-line" => "-next-line",
                        _ => "",
                    },
                    s.mutators.join(", "),
                    s.reason,
                    s.mutants
                );
            }
        }
    }
    out
}

/// Each target's byte range in `source`, when the analysis finds it.
fn byte_ranges(
    targets: &[Target],
    source: &str,
    language: Language,
) -> Vec<Option<(usize, usize)>> {
    let functions = dog_food_core::analyze_source(source, language).unwrap_or_default();
    targets
        .iter()
        .map(|t| {
            functions
                .iter()
                .find(|f| f.name == t.name && f.start_line == t.start_line)
                .map(|f| (f.start_byte, f.end_byte))
        })
        .collect()
}

/// The smallest target around `mutant`: by bytes where they are known,
/// which tells apart functions on the same lines (two lambdas or two
/// accessors on one line), else by lines.
fn innermost(
    targets: &[Target],
    bytes: &[Option<(usize, usize)>],
    mutant: &Mutant,
) -> Option<usize> {
    let size = |i: usize| bytes[i].map_or(usize::MAX, |(start, end)| end - start);
    (0..targets.len())
        .filter(|&i| contains(&targets[i], bytes[i], mutant))
        .min_by_key(|&i| (size(i), targets[i].end_line - targets[i].start_line))
}

fn contains(target: &Target, bytes: Option<(usize, usize)>, mutant: &Mutant) -> bool {
    match bytes {
        Some((start, end)) => start <= mutant.start_byte && mutant.end_byte <= end,
        None => target.start_line <= mutant.line && mutant.line <= target.end_line,
    }
}

pub fn render(targets: &[Target], format: Format, show: bool) -> Result<String, String> {
    match format {
        Format::Table => Ok(table(targets, show)),
        Format::Json => {
            Ok(serde_json::to_string_pretty(&json(targets)).expect("mutants serialize") + "\n")
        }
        Format::Agent => Ok(agent(targets)),
        _ => Err("dgf mutate supports --format table, json or agent".into()),
    }
}

fn json(targets: &[Target]) -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 3,
        "generatedAt": now(),
        "functions": targets.len(),
        "mutants": total(targets),
        "byMutator": by_mutator(targets.iter().flat_map(|t| &t.mutants)),
        "targets": targets,
    })
}

fn total(targets: &[Target]) -> usize {
    targets.iter().map(|t| t.mutants.len()).sum()
}

/// "relational 4, logical 2"
fn by_mutator<'a>(mutants: impl Iterator<Item = &'a Mutant>) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for mutant in mutants {
        *counts.entry(mutant.mutator).or_insert(0) += 1;
    }
    counts
}

fn counts_text(counts: &BTreeMap<&'static str, usize>) -> String {
    counts
        .iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn table(targets: &[Target], show: bool) -> String {
    let width = targets
        .iter()
        .map(|t| t.name.len())
        .max()
        .unwrap_or(8)
        .clamp(8, 50);
    let mut out = format!(
        "{:>7}  {:>10}  {:<8}  {:<width$}  LOCATION\n",
        "MUTANTS", "COMPLEXITY", "WHY", "FUNCTION"
    );
    for t in targets {
        let _ = writeln!(
            out,
            "{:>7}  {:>10}  {:<8}  {:<width$}  {}:{}-{}",
            t.mutants.len(),
            t.complexity,
            t.reason,
            t.name,
            t.file,
            t.start_line,
            t.end_line
        );
        if show {
            for m in &t.mutants {
                let ignored = m
                    .ignored
                    .as_ref()
                    .map(|why| format!(", ignored: {why}"))
                    .unwrap_or_default();
                let _ = writeln!(
                    out,
                    "{:>31}{}:{}  {}  ->  {}   ({}{ignored})",
                    "",
                    m.line,
                    m.column,
                    short(&m.original),
                    m.replacement,
                    m.mutator
                );
            }
        }
    }
    let suppressed = suppressions_text(targets, "  ");
    if !suppressed.is_empty() {
        let _ = write!(out, "\nTurned off by dgf-disable comments:\n{suppressed}");
    }
    let ignored = targets
        .iter()
        .flat_map(|t| &t.mutants)
        .filter(|m| m.ignored.is_some())
        .count();
    let _ = writeln!(
        out,
        "\n{} mutants in {} functions ({}); {ignored} of them ignored\nadd --run to test them (C#, TypeScript/JavaScript)",
        total(targets),
        targets.len(),
        counts_text(&by_mutator(targets.iter().flat_map(|t| &t.mutants)))
    );
    out
}

/// Compact text for AI assistants: one line per function, one per mutant.
fn agent(targets: &[Target]) -> String {
    let mut out = format!("dgf mutate: {} mutants in {} functions\ncolumns: mutants complexity why location name\n        mutant lines: line:column original -> replacement (mutator)\n", total(targets), targets.len());
    for t in targets {
        let _ = writeln!(
            out,
            "{} {} {} {}:{}-{} {}",
            t.mutants.len(),
            t.complexity,
            t.reason,
            t.file,
            t.start_line,
            t.end_line,
            t.name
        );
        for m in &t.mutants {
            let ignored = if m.ignored.is_some() { " ignored" } else { "" };
            let _ = writeln!(
                out,
                "  {}:{} {} -> {} ({}){ignored}",
                m.line,
                m.column,
                short(&m.original),
                m.replacement,
                m.mutator
            );
        }
    }
    let suppressed = suppressions_text(targets, "");
    if !suppressed.is_empty() {
        let _ = write!(out, "suppressed (dgf-disable comments):\n{suppressed}");
    }
    out
}

/// Counts per outcome for a set of mutants.
#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tally {
    pub killed: usize,
    pub survived: usize,
    pub timeout: usize,
    pub no_coverage: usize,
    pub compile_error: usize,
    pub runtime_error: usize,
    /// Mutants turned off: `dgf-disable`, ignore_methods, `[ExcludeFromCodeCoverage]`.
    pub ignored: usize,
    /// Mutants `--max-minutes` left untested.
    pub untested: usize,
}

impl Tally {
    fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Killed => self.killed += 1,
            Outcome::Survived => self.survived += 1,
            Outcome::Timeout => self.timeout += 1,
            Outcome::NoCoverage => self.no_coverage += 1,
            other => self.add_left_out(other),
        }
    }

    /// Outcomes the score leaves out.
    fn add_left_out(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::CompileError => self.compile_error += 1,
            Outcome::RuntimeError => self.runtime_error += 1,
            _ => self.ignored += 1,
        }
    }

    /// Detected ÷ all valid mutants: mutants no test
    /// reaches count against the score; ones that do not compile, crashed
    /// the test run or were not tested are left out.
    pub fn score(&self) -> Option<f64> {
        let detected = self.killed + self.timeout;
        let valid = detected + self.survived + self.no_coverage;
        (valid > 0).then(|| detected as f64 * 100.0 / valid as f64)
    }
}

fn tally<'a>(mutants: impl Iterator<Item = &'a Mutant>, result: &RunResult) -> Tally {
    let mut tally = Tally::default();
    for mutant in mutants {
        match result.outcomes.get(&mutant.id) {
            Some(outcome) => tally.add(*outcome),
            None => tally.untested += usize::from(result.untested.contains_key(&mutant.id)),
        }
    }
    tally
}

fn percent(score: Option<f64>) -> String {
    score.map_or_else(|| "–".to_string(), |s| format!("{s:.0}%"))
}

/// The overall mutation score, if any mutant counted.
pub fn overall(targets: &[Target], result: &RunResult) -> Option<f64> {
    tally(gated(targets), result).score()
}

pub fn render_run(
    targets: &[Target],
    result: &RunResult,
    format: Format,
    thresholds: &Thresholds,
) -> Result<String, String> {
    match format {
        Format::Table => Ok(run_table(targets, result, thresholds)),
        Format::Json => Ok(
            serde_json::to_string_pretty(&run_json(targets, result, thresholds))
                .expect("results serialize")
                + "\n",
        ),
        Format::Agent => Ok(run_agent(targets, result)),
        Format::Html => Ok(crate::report::mutation::render(&run_json(
            targets, result, thresholds,
        ))),
        Format::MutationReport => Ok(serde_json::to_string_pretty(&mutation_report(
            targets, result, thresholds,
        ))
        .expect("results serialize")
            + "\n"),
        _ => Err(
            "dgf mutate --run supports --format table, json, html, agent or mutation-report".into(),
        ),
    }
}

/// The mutation-testing-elements report (schema 2): one entry per file with its source and mutants. Lines and columns
/// are 1-based; the end is exclusive.
fn mutation_report(
    targets: &[Target],
    result: &RunResult,
    thresholds: &Thresholds,
) -> serde_json::Value {
    let mut files: BTreeMap<&str, (Option<PathBuf>, Vec<serde_json::Value>)> = BTreeMap::new();
    for t in targets {
        let entry = files.entry(&t.file).or_default();
        entry.0.get_or_insert_with(|| t.path.clone());
        entry
            .1
            .extend(t.mutants.iter().map(|m| report_mutant(m, result)));
    }
    let files: serde_json::Map<String, serde_json::Value> = files
        .into_iter()
        .map(|(file, (path, mutants))| {
            let path = path.unwrap_or_default();
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            let entry = serde_json::json!({
                "language": report_language(&path),
                "source": source,
                "mutants": mutants,
            });
            (file.to_string(), entry)
        })
        .collect();
    serde_json::json!({
        "schemaVersion": "2",
        "thresholds": { "high": thresholds.high.round(), "low": thresholds.low.round() },
        "files": files,
        "framework": { "name": "dog-food", "version": env!("CARGO_PKG_VERSION") },
        "performance": {
            "setup": (result.prepare + result.build).as_millis() as u64,
            "initialRun": result.baseline.as_millis() as u64,
            "mutation": result.mutants.as_millis() as u64,
        },
    })
}

fn report_mutant(m: &Mutant, result: &RunResult) -> serde_json::Value {
    let (status, reason) = report_status(m, result);
    let mut mutant = serde_json::json!({
        "id": m.id.to_string(),
        "mutatorName": m.mutator,
        "replacement": m.replacement,
        "location": {
            "start": { "line": m.line, "column": m.column },
            "end": { "line": m.end_line, "column": m.end_column },
        },
        "status": status,
    });
    if let Some(reason) = reason {
        mutant["statusReason"] = reason.into();
    }
    mutant
}

/// The schema's status for a mutant, and why when the status needs it.
fn report_status(m: &Mutant, result: &RunResult) -> (&'static str, Option<String>) {
    let Some(outcome) = result.outcomes.get(&m.id) else {
        return (
            "Pending",
            result
                .untested
                .get(&m.id)
                .map(|s| untested_why(*s).to_string()),
        );
    };
    let status = STATUSES
        .iter()
        .find(|(o, _)| o == outcome)
        .map_or("Pending", |(_, name)| name);
    (status, m.ignored.clone())
}

/// The schema's name for each outcome.
const STATUSES: &[(Outcome, &str)] = &[
    (Outcome::Killed, "Killed"),
    (Outcome::Survived, "Survived"),
    (Outcome::Timeout, "Timeout"),
    (Outcome::NoCoverage, "NoCoverage"),
    (Outcome::CompileError, "CompileError"),
    (Outcome::RuntimeError, "RuntimeError"),
    (Outcome::Ignored, "Ignored"),
];

fn report_language(path: &Path) -> &'static str {
    match Language::from_path(path) {
        Some(Language::CSharp) => "csharp",
        Some(Language::Rust) => "rust",
        _ if path
            .extension()
            .is_some_and(|e| e == "js" || e == "jsx" || e == "mjs" || e == "cjs") =>
        {
            "javascript"
        }
        _ => "typescript",
    }
}

/// The agent format's reason: one word, then `boundary-risk` when it applies.
fn agent_why(result: &RunResult, m: &Mutant) -> String {
    let reason = if result.outcomes.get(&m.id) == Some(&Outcome::NoCoverage) {
        "no_test_runs_this"
    } else {
        "tests_still_pass"
    };
    let mut why = reason.to_string();
    if m.boundary_risk {
        why.push_str(" boundary-risk");
    }
    // A cached survivor was not tested in this run: new tests outside the
    // chosen test projects and filter do not change it.
    if result.cached.contains(&m.id) {
        why.push_str(" cached");
    }
    why
}

/// The `tests:` line of a survivor with no reaching tests on record.
fn unreached_text(result: &RunResult, m: &Mutant) -> String {
    if result.outcomes.get(&m.id) == Some(&Outcome::NoCoverage) {
        "none reaches this; write one that calls the function".into()
    } else if result.cached.contains(&m.id) {
        "not recorded (a cached result from an older dgf); --rerun-survivors tests it again and lists them".into()
    } else {
        "not recorded; the tests ran it but none reported reaching it".into()
    }
}

/// What kind of assertion catches a mutant of this kind.
fn assertion_hint(mutator: &str) -> &'static str {
    HINTS
        .iter()
        .find(|(mutators, _)| mutators.contains(&mutator))
        .map_or("both outcomes of this condition", |(_, hint)| hint)
}

const HINTS: &[(&[&str], &str)] = &[
    (&["relational"], "the result at the boundary value, where the two sides are equal"),
    (&["equality"], "one case where the sides are equal and one where they differ"),
    (&["logical"], "a case where only one side of the && / || holds"),
    (
        &["arithmetic", "unary", "update", "assignment", "checked", "math"],
        "the exact computed value",
    ),
    (
        &["statement", "call", "block"],
        "the effect of this code: the state it changes, the call it makes or the exception it throws",
    ),
    (&["string", "stringmethod", "method"], "the exact text"),
    (&["linq"], "which elements come back: order, first or last, empty input"),
    (
        &["nullcoalescing", "optional"],
        "the result both when the value is null and when it is not",
    ),
    (
        &["initializer", "array", "object", "collection"],
        "the contents of what is created",
    ),
    (&["arrow"], "the value the function returns"),
];

/// Mutants the tests did not notice, in source order.
fn survivors<'a>(
    targets: &'a [Target],
    result: &'a RunResult,
) -> impl Iterator<Item = (&'a Target, &'a Mutant)> {
    targets.iter().flat_map(move |t| {
        t.mutants
            .iter()
            .filter(move |m| {
                matches!(
                    result.outcomes.get(&m.id),
                    Some(Outcome::Survived | Outcome::NoCoverage)
                )
            })
            .map(move |m| (t, m))
    })
}

fn why(result: &RunResult, mutant: &Mutant) -> &'static str {
    match (
        result.outcomes.get(&mutant.id) == Some(&Outcome::NoCoverage),
        mutant.boundary_risk,
    ) {
        (true, false) => "no test runs this",
        (true, true) => "no test runs this, boundary-risk",
        (false, false) => "tests still pass",
        (false, true) => "tests still pass, boundary-risk",
    }
}

fn run_table(targets: &[Target], result: &RunResult, thresholds: &Thresholds) -> String {
    let width = targets
        .iter()
        .map(|t| t.name.len())
        .max()
        .unwrap_or(8)
        .clamp(8, 50);
    let mut out = format!(
        "{:>5}  {:>6}  {:>8}  {:>8}  {:>7}  {:<width$}  LOCATION\n",
        "SCORE", "KILLED", "SURVIVED", "NO TESTS", "TIMEOUT", "FUNCTION"
    );
    for t in targets.iter().filter(|t| !t.mutants.is_empty()) {
        let n = tally(t.mutants.iter(), result);
        let _ = writeln!(
            out,
            "{:>5}  {:>6}  {:>8}  {:>8}  {:>7}  {:<width$}  {}:{}-{}",
            percent(n.score()),
            n.killed,
            n.survived,
            n.no_coverage,
            n.timeout,
            t.name,
            t.file,
            t.start_line,
            t.end_line
        );
    }
    out.push_str("\nNot caught by any test (add or sharpen a test for each):\n");
    for (t, m) in survivors(targets, result) {
        let _ = writeln!(
            out,
            "  {}:{}:{}  {}  ->  {}   ({}, {})  in {}",
            t.file,
            m.line,
            m.column,
            short(&m.original),
            m.replacement,
            m.mutator,
            why(result, m),
            t.name
        );
    }
    let suppressed = suppressions_text(targets, "  ");
    if !suppressed.is_empty() {
        let _ = write!(out, "\nTurned off by dgf-disable comments:\n{suppressed}");
    }
    out.push_str(&untested_text(targets, result));
    for note in &result.notes {
        let _ = writeln!(out, "\nnote: {note}");
    }
    out.push_str(&summary(targets, result, thresholds));
    out
}

/// What `--max-minutes` left untested, per step that did not finish.
fn untested_text(targets: &[Target], result: &RunResult) -> String {
    if !result.budget_hit() {
        return String::new();
    }
    let stopped = |step: &Untested| !matches!(step, Untested::NoRunner | Untested::NotCached);
    let mut out = format!(
        "\nThe time budget ran out; {} mutants were not tested (the results so far are saved):\n",
        result.untested.values().filter(|s| stopped(s)).count()
    );
    for (t, m) in targets
        .iter()
        .flat_map(|t| t.mutants.iter().map(move |m| (t, m)))
    {
        if let Some(step) = result.untested.get(&m.id).filter(|s| stopped(s)) {
            let _ = writeln!(
                out,
                "  {}:{}:{}  {}  ->  {}   ({}, {})  in {}",
                t.file,
                m.line,
                m.column,
                short(&m.original),
                m.replacement,
                m.mutator,
                untested_why(*step),
                t.name
            );
        }
    }
    out
}

fn untested_why(step: Untested) -> &'static str {
    match step {
        Untested::Build => "stopped while building",
        Untested::Baseline => "stopped during the baseline test run",
        Untested::TestRuns => "its test runs did not finish",
        Untested::NoRunner => "no vitest or jest package above this file",
        Untested::NotCached => "not in the cache (--cached tests nothing)",
    }
}

/// Where a score falls among the thresholds, in words.
fn band(score: Option<f64>, thresholds: &Thresholds) -> String {
    let Some(score) = score else {
        return "no mutant counted".into();
    };
    let failed = thresholds
        .fail
        .filter(|limit| score < *limit)
        .map(|limit| format!(", below break {limit:.0}: fails"))
        .unwrap_or_default();
    let name = if score >= thresholds.high {
        "high"
    } else if score >= thresholds.low {
        "medium"
    } else {
        "low"
    };
    format!("{name}{failed}")
}

fn summary(targets: &[Target], result: &RunResult, thresholds: &Thresholds) -> String {
    let n = tally(gated(targets), result);
    let sandboxes: Vec<String> = result
        .sandboxes
        .iter()
        .map(|s| s.display().to_string())
        .collect();
    format!(
        "\nmutation score {} ({}) · {} killed, {} survived, {} no tests, {} timeout, {} did not compile, {} crashed the test run, {} ignored, {} not tested; {} from the cache\ntime: prepare {:.1} s · build {:.1} s · baseline tests {:.1} s · {} test runs for the mutants {:.1} s on {} workers\nsandbox: {}\n",
        percent(n.score()),
        band(n.score(), thresholds),
        n.killed,
        n.survived,
        n.no_coverage,
        n.timeout,
        n.compile_error,
        n.runtime_error,
        n.ignored,
        n.untested,
        result.cached.len(),
        result.prepare.as_secs_f64(),
        result.build.as_secs_f64(),
        result.baseline.as_secs_f64(),
        result.test_runs,
        result.mutants.as_secs_f64(),
        result.jobs,
        if sandboxes.is_empty() { "none (everything came from the cache)".to_string() } else { sandboxes.join(", ") }
    )
}

fn run_json(targets: &[Target], result: &RunResult, thresholds: &Thresholds) -> serde_json::Value {
    let functions: Vec<serde_json::Value> =
        targets.iter().map(|t| function_json(t, result)).collect();
    serde_json::json!({
        "thresholds": { "high": thresholds.high, "low": thresholds.low, "break": thresholds.fail },
        "schemaVersion": 4,
        "generatedAt": now(),
        "score": overall(targets, result),
        "tally": tally(gated(targets), result),
        "budgetHit": result.budget_hit(),
        "notes": result.notes,
        "timings": {
            "prepareMs": result.prepare.as_millis(),
            "buildMs": result.build.as_millis(),
            "baselineMs": result.baseline.as_millis(),
            "mutantsMs": result.mutants.as_millis(),
            "testRuns": result.test_runs,
            "retries": result.retries,
            "jobs": result.jobs,
            "fromCacheOf": result.timings_from,
        },
        "cached": result.cached.len(),
        "testProjects": result.test_projects,
        "sandboxes": result.sandboxes,
        "functions": functions,
    })
}

fn function_json(t: &Target, result: &RunResult) -> serde_json::Value {
    let mutants: Vec<serde_json::Value> = t
        .mutants
        .iter()
        .map(|m| serde_json::json!({ "mutant": m, "outcome": result.outcomes.get(&m.id), "cached": result.cached.contains(&m.id), "untested": result.untested.get(&m.id) }))
        .collect();
    let counts = tally(t.mutants.iter(), result);
    serde_json::json!({
        "file": t.file,
        "name": t.name,
        "startLine": t.start_line,
        "endLine": t.end_line,
        "complexity": t.complexity,
        "reason": t.reason,
        "tally": counts,
        "score": counts.score(),
        "mutants": mutants,
        "suppressions": t.suppressions,
    })
}

/// Kills from runs whose test host crashed: a failing test counted, but the
/// crash may have caused it, so an equivalent mutant can pass for covered.
fn crash_kills_text(targets: &[Target], result: &RunResult) -> String {
    let mut out = String::new();
    for (t, m) in targets
        .iter()
        .flat_map(|t| t.mutants.iter().map(move |m| (t, m)))
        .filter(|(_, m)| result.crash_kills.contains(&m.id))
    {
        let _ = writeln!(
            out,
            "{}:{}:{} {} {} -> {} ({})",
            t.file,
            m.line,
            m.column,
            t.name,
            short(&m.original),
            m.replacement,
            m.mutator
        );
    }
    if out.is_empty() {
        return out;
    }
    format!("killed in a run whose test host crashed (a failing test counted, but the crash may have caused it; check these by hand before calling one covered):\n{out}")
}

/// One survivor in the agent format: where, what, which tests reach it and
/// what to assert.
fn agent_survivor(out: &mut String, t: &Target, m: &Mutant, result: &RunResult) {
    let _ = writeln!(
        out,
        "{}:{}:{} {} {} -> {} ({}) {}",
        t.file,
        m.line,
        m.column,
        t.name,
        short(&m.original),
        m.replacement,
        m.mutator,
        agent_why(result, m)
    );
    let tests = result.reached.get(&m.id).filter(|t| !t.is_empty());
    let hint = if m.boundary_risk {
        "a date boundary: the result on the boundary day itself (the off-by-one-day bug); never call this one equivalent"
    } else {
        assertion_hint(m.mutator)
    };
    let _ = writeln!(
        out,
        "  tests: {}\n  assert: {hint}",
        tests.map_or_else(|| unreached_text(result, m), |t| t.join(", ")),
    );
}

/// Compact text for AI assistants: the score, then every surviving mutant.
fn run_agent(targets: &[Target], result: &RunResult) -> String {
    let n = tally(gated(targets), result);
    let mut out = format!(
        "dgf mutate: score {} killed={} survived={} no_tests={} timeout={} compile_error={} runtime_error={} ignored={} untested={}\n",
        percent(n.score()),
        n.killed,
        n.survived,
        n.no_coverage,
        n.timeout,
        n.compile_error,
        n.runtime_error,
        n.ignored,
        n.untested
    );
    if result.budget_hit() {
        out.push_str("budget: the time budget ran out; run again to test the rest (results so far are cached)\n");
    }
    out.push_str("survivors: each is a change no test noticed; add a test that fails for it\n");
    out.push_str("columns: location function original -> replacement (mutator) why\n");
    out.push_str("        why ends in `cached` when this run took the result from the cache: new tests outside the chosen test projects and filter do not change it\n");
    out.push_str(
        "        then: tests that reach it (where a new assertion goes), and what to assert\n",
    );
    let (neighbours, gate): (Vec<_>, Vec<_>) =
        survivors(targets, result).partition(|(t, _)| t.reason == NEIGHBOUR);
    let (by_tests, own): (Vec<_>, Vec<_>) =
        gate.into_iter().partition(|(t, _)| t.reason == "tests");
    for (t, m) in own {
        agent_survivor(&mut out, t, m, result);
    }
    if !by_tests.is_empty() {
        out.push_str("\n## in code a test file this branch changed reaches (not in the branch's own diff; counted in the score)\n");
        for (t, m) in by_tests {
            agent_survivor(&mut out, t, m, result);
        }
    }
    if !neighbours.is_empty() {
        out.push_str("
## next to your change (--neighbours: code that holds a string key of a changed function; not in the score, yours to decide)
");
        for (t, m) in neighbours {
            agent_survivor(&mut out, t, m, result);
        }
    }
    out.push_str(&crash_kills_text(targets, result));
    let suppressed = suppressions_text(targets, "");
    if !suppressed.is_empty() {
        let _ = write!(
            out,
            "suppressed (dgf-disable comments; not tested):\n{suppressed}"
        );
    }
    for note in &result.notes {
        let _ = writeln!(out, "note: {note}");
    }
    out.push_str("\n## per function (score killed survived no_tests location name)\n");
    for t in targets.iter().filter(|t| !t.mutants.is_empty()) {
        let f = tally(t.mutants.iter(), result);
        let _ = writeln!(
            out,
            "{} {} {} {} {}:{}-{} {}",
            percent(f.score()),
            f.killed,
            f.survived,
            f.no_coverage,
            t.file,
            t.start_line,
            t.end_line,
            t.name
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mutation_report_follows_its_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Shop.cs");
        std::fs::write(
            &path,
            "class C {\n  int F(int a) {\n    // dgf-disable-once arithmetic: units only\n    var b = a * 2;\n    return a > 1 ? b : 0;\n  }\n}\n",
        )
        .unwrap();
        let wanted = Wanted {
            file: "Shop.cs".into(),
            path: path.clone(),
            name: "C.F".into(),
            start_line: 2,
            end_line: 6,
            complexity: 2,
            reason: "selected",
            lines: None,
        };
        let (mut targets, _) = targets(vec![wanted], &Settings::default()).unwrap();
        for (id, m) in targets[0].mutants.iter_mut().enumerate() {
            m.id = id;
        }
        let mut result = RunResult::default();
        let outcomes = [Outcome::Killed, Outcome::Survived, Outcome::Timeout];
        for (m, outcome) in targets[0].mutants.iter().zip(outcomes.iter().cycle()) {
            let outcome = if m.ignored.is_some() {
                Outcome::Ignored
            } else {
                *outcome
            };
            result.outcomes.insert(m.id, outcome);
        }
        let last = targets[0].mutants.last().unwrap().id;
        result.outcomes.remove(&last);
        result.untested.insert(last, Untested::TestRuns);
        let report = mutation_report(&targets, &result, &Thresholds::default());
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../tests/schema/mutation-report.schema.json"))
                .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&report)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{errors:#?}\n{report:#}");
        let mutants = report["files"]["Shop.cs"]["mutants"].as_array().unwrap();
        assert!(mutants.iter().any(|m| m["status"] == "Ignored"));
        assert!(mutants.iter().any(|m| m["status"] == "Pending"));
        let relational = mutants
            .iter()
            .find(|m| m["mutatorName"] == "relational")
            .unwrap();
        assert_eq!(relational["location"]["start"]["line"], 5);
    }

    #[test]
    fn keys_are_quoted_names_and_the_constants_that_hold_them() {
        assert_eq!(
            literals(r#"input["MemberPlanId"] = id; log("saved it"); x = 'Ab';"#),
            [r#""MemberPlanId""#]
        );
        assert_eq!(
            constant_name(r#"    public const string MemberPlanKey = "MemberPlanId";"#),
            Some("MemberPlanKey")
        );
        assert_eq!(constant_name(r#"    var key = "MemberPlanId";"#), None);
        assert!(has_word("[InputKeys.MemberPlanKey] = id", "MemberPlanKey"));
        assert!(!has_word("MemberPlanKeys.Add(x)", "MemberPlanKey"));
    }

    #[test]
    fn neighbours_hold_a_key_of_the_change_on_their_own_lines() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let lookup = root.join("Lookup.cs");
        std::fs::write(
            &lookup,
            "class Lookup {
  public const string PlanKey = \"MemberPlanId\";
  object Read(Dictionary<string, object> input) {
    return input[PlanKey];
  }
}
",
        )
        .unwrap();
        let handler = root.join("Handler.cs");
        std::fs::write(
            &handler,
            "class Handler {
  void Start(Dictionary<string, object> input, int id) {
    var other = id + 1;
    input[\"MemberPlanId\"] = id;
  }
  int Unrelated(int a) { return a * 2; }
}
",
        )
        .unwrap();
        let wanted = Wanted {
            file: "Lookup.cs".into(),
            path: lookup.clone(),
            name: "Lookup.Read".into(),
            start_line: 3,
            end_line: 5,
            complexity: 1,
            reason: "worse",
            lines: None,
        };
        let (changed, _) = targets(vec![wanted], &Settings::default()).unwrap();
        let keys = Keys::of(&changed);
        assert!(keys.literals.contains(r#""MemberPlanId""#), "{keys:?}");
        assert!(keys.names.contains("PlanKey"));
        let taken = HashSet::from([("Lookup.cs".to_string(), 3)]);
        let near = neighbours(&[lookup, handler], root, &keys, &taken);
        assert_eq!(near.len(), 1);
        assert_eq!(
            (near[0].file.as_str(), near[0].name.as_str()),
            ("Handler.cs", "Handler.Start")
        );
        assert_eq!(near[0].lines, Some(vec![(4, 4)]));
        assert!(near.iter().all(|w| w.reason == NEIGHBOUR));
    }

    #[test]
    fn kills_from_a_crashed_run_are_listed_for_a_check_by_hand() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Report.cs");
        std::fs::write(
            &path,
            "class R {\n  bool F(object s) {\n    return s is null;\n  }\n}\n",
        )
        .unwrap();
        let wanted = Wanted {
            file: "Report.cs".into(),
            path,
            name: "R.F".into(),
            start_line: 2,
            end_line: 4,
            complexity: 1,
            reason: "selected",
            lines: None,
        };
        let (mut targets, _) = targets(vec![wanted], &Settings::default()).unwrap();
        for (id, m) in targets[0].mutants.iter_mut().enumerate() {
            m.id = id;
        }
        let mut result = RunResult::default();
        assert_eq!(crash_kills_text(&targets, &result), "");
        result.crash_kills.insert(0);
        let text = crash_kills_text(&targets, &result);
        assert!(
            text.starts_with("killed in a run whose test host crashed"),
            "{text}"
        );
        assert!(text.contains("Report.cs:3:"), "{text}");
    }

    #[test]
    fn constructors_that_only_store_their_arguments_are_plain() {
        assert!(is_constructor("Orders.Handler.Handler"));
        assert!(!is_constructor("Handler.Handle"));
        let plain = [
            "public Handler(IRepo repo, ILogger log)",
            "{",
            "    _repo = repo;",
            "    this._log = log ?? throw new ArgumentNullException(nameof(log));",
            "}",
        ];
        assert!(only_stores(&plain));
        let busy = [
            "public Handler(IRepo repo) {",
            "    _repo = repo;",
            "    _cache = repo.Load();",
            "}",
        ];
        assert!(!only_stores(&busy));
        assert!(!only_stores(&["public Handler() { }"]));
    }

    #[test]
    fn test_code_is_never_mutated() {
        for test in [
            "src/pricing.spec.ts",
            "web/a.test.tsx",
            "web/__tests__/a.ts",
            "Shop.Tests/DiscountTests.cs",
            "tests/Helpers.cs",
            "src/OrderTest.cs",
        ] {
            assert!(is_test_code(Path::new(test)), "{test}");
        }
        for code in [
            "src/pricing.ts",
            "Shop/Latest.cs",
            "src/Contest.ts",
            "Shop/Testing.cs",
        ] {
            assert!(!is_test_code(Path::new(code)), "{code}");
        }
    }

    #[test]
    fn functions_on_one_line_get_their_own_mutants() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.ts");
        let source = "export function f(xs: number[]) {\n  return xs.filter((x) => x > 1).map((y) => y * 2);\n}\n";
        std::fs::write(&path, source).unwrap();
        let functions =
            dog_food_core::analyze_source(source, dog_food_core::Language::TypeScript).unwrap();
        let wanted: Vec<Wanted> = functions
            .iter()
            .filter(|f| f.start_line == 2)
            .map(|f| Wanted {
                file: "b.ts".into(),
                path: path.clone(),
                name: f.name.clone(),
                start_line: f.start_line,
                end_line: f.end_line,
                complexity: 1,
                reason: "selected",
                lines: None,
            })
            .collect();
        assert_eq!(wanted.len(), 2);
        let basic = Settings {
            level: dog_food_core::mutate::Level::Basic,
            ..Settings::default()
        };
        let (targets, _) = targets(wanted, &basic).unwrap();
        let originals = |t: &Target| {
            t.mutants
                .iter()
                .map(|m| m.original.clone())
                .collect::<Vec<_>>()
        };
        let mut all: Vec<Vec<String>> = targets.iter().map(originals).collect();
        all.sort();
        assert_eq!(
            all,
            [
                vec!["*".to_string()],
                vec![">".to_string(), ">".to_string()]
            ]
        );
    }

    #[test]
    fn mutants_go_to_the_innermost_chosen_function() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.ts");
        std::fs::write(&path, "function outer(a: number) {\n  const inner = (b: number) => b > 1;\n  return a + 1;\n}\n").unwrap();
        let wanted = |name: &str, start, end| Wanted {
            file: "a.ts".into(),
            path: path.clone(),
            name: name.into(),
            start_line: start,
            end_line: end,
            complexity: 1,
            reason: "selected",
            lines: None,
        };
        let basic = Settings {
            level: dog_food_core::mutate::Level::Basic,
            ignore_mutators: vec!["block".into()],
            ..Settings::default()
        };
        let (targets, warnings) = targets(
            vec![wanted("outer", 1, 4), wanted("outer/inner", 2, 2)],
            &basic,
        )
        .unwrap();
        assert!(warnings.is_empty());
        let find = |name: &str| targets.iter().find(|t| t.name == name).unwrap();
        assert_eq!(
            find("outer/inner")
                .mutants
                .iter()
                .map(|m| m.original.as_str())
                .collect::<Vec<_>>(),
            [">", ">"]
        );
        assert_eq!(
            find("outer")
                .mutants
                .iter()
                .map(|m| m.original.as_str())
                .collect::<Vec<_>>(),
            ["+"]
        );
        let text = render(&targets, Format::Table, true).unwrap();
        assert!(
            text.contains(
                "3 mutants in 2 functions (arithmetic 1, relational 2); 0 of them ignored"
            ),
            "{text}"
        );
        assert!(text.contains("2:34  >  ->  >=   (relational)"), "{text}");
        assert!(render(&targets, Format::Html, false).is_err());
    }
}
