//! What a branch and its uncommitted edits did to complexity, compared with
//! the commit where the branch left its base (usually `main`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use dog_food_core::{FunctionMetrics, Language};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::discover::Excludes;
use crate::git::{self, FileChange};
use crate::limits::{Level, Limits, Status};

/// Which functions in a changed file are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Functions that contain a changed line
    Lines,
    /// Every function in a changed file
    Files,
}

/// Which reported functions can fail the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum FailOn {
    /// New functions and functions whose score went up
    NewOrWorse,
    /// Every reported function, including ones whose score did not change
    Touched,
}

impl FailOn {
    fn counts(self, change: Change) -> bool {
        match self {
            FailOn::NewOrWorse => matches!(change, Change::New | Change::Worse),
            FailOn::Touched => change != Change::Removed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    New,
    Worse,
    Better,
    Same,
    Removed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFunction {
    pub change: Change,
    pub file: String,
    pub name: String,
    pub kind: &'static str,
    pub start_line: usize,
    pub end_line: usize,
    pub before: Option<u32>,
    pub after: Option<u32>,
    pub status: Status,
    pub limits: Level,
    /// Whether this function can fail the run (see `FailOn`).
    pub counted: bool,
    pub breakdown: BTreeMap<&'static str, u32>,
}

impl ChangedFunction {
    pub fn delta(&self) -> i64 {
        i64::from(self.after.unwrap_or(0)) - i64::from(self.before.unwrap_or(0))
    }
}

/// Everything `dgf changes` found.
pub struct ChangesRun {
    pub base: String,
    pub base_commit: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub scope: Scope,
    pub fail_on: FailOn,
    pub limits: Level,
    pub files: Vec<String>,
    pub rows: Vec<ChangedFunction>,
    pub elapsed: Duration,
    pub warnings: Vec<String>,
}

impl ChangesRun {
    pub fn count(&self, change: Change) -> usize {
        self.rows.iter().filter(|row| row.change == change).count()
    }

    /// The worst status among the functions that can fail the run.
    pub fn verdict(&self) -> Status {
        self.counted(None)
            .map(|row| row.status)
            .max()
            .unwrap_or(Status::Ok)
    }

    /// Functions that can fail the run, optionally only those with `status`.
    pub fn counted(&self, status: Option<Status>) -> impl Iterator<Item = &ChangedFunction> {
        self.rows
            .iter()
            .filter(move |row| row.counted && status.is_none_or(|s| row.status == s))
    }

    /// Sum of all score changes: positive means the code got more complex.
    pub fn net(&self) -> i64 {
        self.rows.iter().map(ChangedFunction::delta).sum()
    }
}

/// What to compare and how.
pub struct Request<'a> {
    pub root: &'a Path,
    pub base: String,
    /// Compare the staged content instead of the working tree.
    pub staged: bool,
    pub scope: Scope,
    pub fail_on: FailOn,
    pub excludes: &'a Excludes,
    pub limits: &'a Limits,
}

pub fn collect(request: Request) -> Result<ChangesRun, String> {
    let started = Instant::now();
    let (base_commit, base_note) = git::merge_base(request.root, &request.base)?;
    let files: Vec<FileChange> = git::changed_files(request.root, &base_commit, request.staged)?
        .into_iter()
        .filter(|file| is_source(&request, file))
        .collect();
    let results: Vec<_> = files
        .par_iter()
        .map(|file| compare_file(&request, &base_commit, file))
        .collect();

    let mut rows = Vec::new();
    let mut warnings: Vec<String> = base_note.into_iter().collect();
    for result in results {
        match result {
            Ok(mut list) => rows.append(&mut list),
            Err(err) => warnings.push(err),
        }
    }
    rows.sort_by_key(|row| (std::cmp::Reverse(row.delta()), std::cmp::Reverse(row.after)));
    let (head, branch) = git::head(request.root);
    Ok(ChangesRun {
        base_commit: git::short(request.root, &base_commit),
        base: request.base,
        head,
        branch,
        scope: request.scope,
        fail_on: request.fail_on,
        limits: request.limits.global,
        files: files.into_iter().map(|file| file.path).collect(),
        rows,
        elapsed: started.elapsed(),
        warnings,
    })
}

fn is_source(request: &Request, file: &FileChange) -> bool {
    Language::from_path(Path::new(&file.path)).is_some()
        && !request
            .excludes
            .is_excluded(&request.root.join(&file.path), false)
}

fn compare_file(
    request: &Request,
    base_commit: &str,
    file: &FileChange,
) -> Result<Vec<ChangedFunction>, String> {
    let path = request.root.join(&file.path);
    let (now, language) = current(request, file)?;
    let before = previous(request.root, base_commit, file, language);
    let level = request.limits.for_file(&path);
    let touched = |f: &FunctionMetrics| {
        request.scope == Scope::Files || file.touches(f.start_line, f.end_line)
    };
    let mut rows = compare(&file.path, &before, &now, touched, level);
    for row in &mut rows {
        row.counted = request.fail_on.counts(row.change);
    }
    Ok(rows)
}

/// The functions of the file now: in the working tree, or as staged.
fn current(
    request: &Request,
    file: &FileChange,
) -> Result<(Vec<FunctionMetrics>, Language), String> {
    let path = request.root.join(&file.path);
    if !request.staged {
        let report = dog_food_core::analyze_file(&path).map_err(|err| err.to_string())?;
        return Ok((report.functions, report.language));
    }
    let language =
        Language::from_path(&path).ok_or_else(|| format!("{}: unsupported file", file.path))?;
    let source = git::staged(request.root, &file.path)
        .ok_or_else(|| format!("{}: not staged", file.path))?;
    let functions =
        dog_food_core::analyze_source(&source, language).map_err(|err| err.to_string())?;
    Ok((functions, language))
}

/// The functions of the file as it was in the base commit.
fn previous(
    root: &Path,
    commit: &str,
    file: &FileChange,
    language: Language,
) -> Vec<FunctionMetrics> {
    file.old_path
        .as_deref()
        .and_then(|old| git::show(root, commit, old))
        .and_then(|source| dog_food_core::analyze_source(&source, language).ok())
        .unwrap_or_default()
}

/// Pairs the functions of the old and new version of one file by name and
/// keeps the touched ones, plus every function that no longer exists.
pub fn compare(
    file: &str,
    before: &[FunctionMetrics],
    after: &[FunctionMetrics],
    touched: impl Fn(&FunctionMetrics) -> bool,
    limits: Level,
) -> Vec<ChangedFunction> {
    let old: HashMap<String, &FunctionMetrics> = keyed(before).into_iter().collect();
    let new = keyed(after);
    let new_keys: HashSet<&String> = new.iter().map(|(key, _)| key).collect();
    let mut rows: Vec<ChangedFunction> = new
        .iter()
        .filter(|(_, f)| touched(f))
        .map(|(key, f)| changed(file, f, old.get(key).map(|b| b.complexity), limits))
        .collect();
    let mut gone: Vec<&FunctionMetrics> = old
        .iter()
        .filter(|(key, _)| !new_keys.contains(key))
        .map(|(_, f)| *f)
        .collect();
    gone.sort_by_key(|f| f.start_line);
    rows.extend(gone.into_iter().map(|f| removed(file, f, limits)));
    rows
}

/// Keys that identify a function across versions: its name, made stable,
/// plus a counter for overloads and anonymous functions with the same name.
pub fn keyed(functions: &[FunctionMetrics]) -> Vec<(String, &FunctionMetrics)> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    functions
        .iter()
        .map(|f| {
            let name = stable_name(&f.name);
            let count = seen.entry(name.clone()).or_insert(0);
            *count += 1;
            (format!("{name}#{count}"), f)
        })
        .collect()
}

/// Drops the `@line:column` of anonymous functions, which changes whenever
/// code above them moves: `run/closure@12:5` becomes `run/closure`.
pub fn stable_name(name: &str) -> String {
    name.split('/')
        .map(|part| part.split_once('@').map_or(part, |(head, _)| head))
        .collect::<Vec<_>>()
        .join("/")
}

fn changed(file: &str, f: &FunctionMetrics, before: Option<u32>, limits: Level) -> ChangedFunction {
    let change = match before {
        None => Change::New,
        Some(old) if f.complexity > old => Change::Worse,
        Some(old) if f.complexity < old => Change::Better,
        Some(_) => Change::Same,
    };
    ChangedFunction {
        change,
        file: file.to_string(),
        name: f.name.clone(),
        kind: f.kind,
        start_line: f.start_line,
        end_line: f.end_line,
        before,
        after: Some(f.complexity),
        status: match f.ignored {
            Some(_) => Status::Ignored,
            None => limits.status(f.complexity),
        },
        limits,
        counted: false,
        breakdown: f.breakdown.clone(),
    }
}

fn removed(file: &str, f: &FunctionMetrics, limits: Level) -> ChangedFunction {
    ChangedFunction {
        change: Change::Removed,
        file: file.to_string(),
        name: f.name.clone(),
        kind: f.kind,
        start_line: f.start_line,
        end_line: f.end_line,
        before: Some(f.complexity),
        after: None,
        status: Status::Ok,
        limits,
        counted: false,
        breakdown: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(name: &str, complexity: u32, start_line: usize) -> FunctionMetrics {
        FunctionMetrics {
            name: name.to_string(),
            kind: "method",
            start_line,
            end_line: start_line + 5,
            complexity,
            ..FunctionMetrics::default()
        }
    }

    const LIMITS: Level = Level {
        warn: Some(5),
        fail: Some(6),
    };

    fn changes(
        before: &[FunctionMetrics],
        after: &[FunctionMetrics],
    ) -> Vec<(String, Change, Option<u32>, Option<u32>)> {
        compare("a.cs", before, after, |_| true, LIMITS)
            .into_iter()
            .map(|r| (r.name, r.change, r.before, r.after))
            .collect()
    }

    #[test]
    fn classifies_new_worse_better_same_and_removed() {
        let before = [
            f("A.Same", 3, 1),
            f("A.Worse", 4, 10),
            f("A.Better", 8, 20),
            f("A.Gone", 2, 30),
        ];
        let after = [
            f("A.Same", 3, 1),
            f("A.Worse", 9, 10),
            f("A.Better", 5, 20),
            f("A.New", 7, 40),
        ];
        assert_eq!(
            changes(&before, &after),
            [
                ("A.Same".into(), Change::Same, Some(3), Some(3)),
                ("A.Worse".into(), Change::Worse, Some(4), Some(9)),
                ("A.Better".into(), Change::Better, Some(8), Some(5)),
                ("A.New".into(), Change::New, None, Some(7)),
                ("A.Gone".into(), Change::Removed, Some(2), None),
            ]
        );
    }

    #[test]
    fn untouched_functions_are_left_out() {
        let before = [f("A.Edited", 2, 1), f("A.Other", 9, 50)];
        let after = [f("A.Edited", 3, 1), f("A.Other", 9, 50)];
        let rows = compare("a.cs", &before, &after, |f| f.start_line < 10, LIMITS);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "A.Edited");
        assert_eq!(rows[0].status, Status::Ok);
    }

    #[test]
    fn anonymous_functions_match_even_when_they_move() {
        let before = [f("run", 1, 1), f("run/closure@3:9", 2, 3)];
        let after = [f("run", 1, 1), f("run/closure@8:9", 2, 8)];
        let found = changes(&before, &after);
        assert!(
            found
                .iter()
                .all(|(_, change, _, _)| *change == Change::Same),
            "{found:?}"
        );
    }

    #[test]
    fn overloads_are_matched_in_order() {
        let before = [f("C.Add", 2, 1), f("C.Add", 3, 10)];
        let after = [f("C.Add", 2, 1), f("C.Add", 5, 10)];
        let found = changes(&before, &after);
        assert_eq!(found[1], ("C.Add".into(), Change::Worse, Some(3), Some(5)));
    }

    #[test]
    fn fail_on_rules() {
        assert!(FailOn::NewOrWorse.counts(Change::Worse));
        assert!(FailOn::NewOrWorse.counts(Change::New));
        assert!(!FailOn::NewOrWorse.counts(Change::Same));
        assert!(FailOn::Touched.counts(Change::Same));
        assert!(!FailOn::Touched.counts(Change::Removed));
    }

    #[test]
    fn stable_names_drop_positions() {
        assert_eq!(stable_name("Cart.Run/abs"), "Cart.Run/abs");
        assert_eq!(
            stable_name("f/<anonymous>@4:2/closure@5:1"),
            "f/<anonymous>/closure"
        );
    }
}
