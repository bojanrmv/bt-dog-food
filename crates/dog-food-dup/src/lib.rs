//! Duplicated code that can be united: copies that are the same apart from
//! names and values, found on the syntax tree, with the template they share,
//! the holes where they differ and what uniting them takes.
//!
//! It runs on its own (`dgf-dup`) and as a library, so `dgf` can put
//! duplication next to complexity, coverage and mutation results.
//!
//! How: every statement is reduced to its shape (keywords, operators and
//! structure; names and literals become placeholders). Runs of statements
//! with the same shapes are seeds; each pair of seeds grows as far as both
//! stay alike, and equal runs form a clone class. Anti-unification of the
//! copies gives the template: a hole wherever their names or values differ,
//! one hole for each distinct set of differences.

pub mod bench;
pub mod changes;
pub mod cli;
pub mod decisions;
mod find;
mod near;
mod reorder;
pub mod report;
pub mod risk;
pub mod semantic;
mod tokens;

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Serialize;

pub use find::{CloneClass, Confirmation, Copy, Difference, Hole, HoleKind, Settings};

/// What a duplication scan found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// With `--changes`: what the findings were limited to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub files: usize,
    /// Lines of code in the files scanned (statements only).
    pub lines: usize,
    /// Lines in copies beyond the first of each class.
    pub duplicated_lines: usize,
    pub classes: Vec<CloneClass>,
    /// Line counts of the files that hold a copy, for drawing them to scale.
    pub file_lines: std::collections::BTreeMap<String, usize>,
    /// The project each file with a copy belongs to: its nearest folder with
    /// a .csproj, package.json or Cargo.toml.
    pub projects: std::collections::BTreeMap<String, String>,
    /// With `--decisions`: classes left out as decided (intentional or a false positive).
    #[serde(skip_serializing_if = "is_zero")]
    pub suppressed: usize,
    /// With `--decisions`: every decision, by class id.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub decisions: std::collections::BTreeMap<String, decisions::Decision>,
}

/// Lines in copies beyond the first of each class; repeated queries are
/// conditions written again, not copied lines, and do not count.
pub(crate) fn duplicated(classes: &[CloneClass]) -> usize {
    classes
        .iter()
        .filter(|c| c.kind != "query")
        .map(|c| c.lines * (c.copies.len() - 1))
        .sum()
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Report {
    /// Only the findings with a copy in lines the branch changed, those
    /// copies marked: a new copy of old code, or code copied within the
    /// branch. The scan still covers everything, so the other copies show.
    ///
    /// With `at_base`, the ids of the groups the merge base already had, a
    /// group the branch only edited (uniting other copies changes lines in
    /// it) is left out too: only what the branch added is reported.
    pub fn keep_changed(
        &mut self,
        changes: &changes::Changes,
        at_base: Option<&std::collections::BTreeSet<String>>,
    ) {
        for class in &mut self.classes {
            for copy in &mut class.copies {
                copy.changed = changes.touches(&copy.file, copy.start_line, copy.end_line);
            }
        }
        self.classes
            .retain(|c| c.copies.iter().any(|copy| copy.changed));
        let touched = self.classes.len();
        if let Some(at_base) = at_base {
            self.classes.retain(|c| !at_base.contains(&c.id));
        }
        let old = touched - self.classes.len();
        let used: std::collections::BTreeSet<String> = self
            .classes
            .iter()
            .flat_map(|c| c.copies.iter().map(|copy| copy.file.clone()))
            .collect();
        self.file_lines.retain(|file, _| used.contains(file));
        self.duplicated_lines = duplicated(&self.classes);
        let old = if old > 0 {
            format!("; {old} groups with a changed copy were at the base already and are left out")
        } else {
            String::new()
        };
        self.scope = Some(format!(
            "findings with a copy in what the branch changed since {} ({}): {} changed files{old}",
            changes.base,
            changes.commit,
            changes.lines.len()
        ));
    }

    /// Names each class by its type and the functions its copies are in, and
    /// finds the project of each file with a copy. A second class in the same
    /// functions gets `-2`, and so on.
    pub fn index(&mut self, root: &Path) {
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for class in &mut self.classes {
            let base = class_id(class);
            let n = seen.entry(base.clone()).or_default();
            *n += 1;
            class.id = if *n == 1 { base } else { format!("{base}-{n}") };
        }
        let mut found = std::collections::HashMap::new();
        for file in self.file_lines.keys() {
            let project = project_of(root, Path::new(file), &mut found);
            self.projects.insert(file.clone(), project);
        }
    }

    /// Duplicated lines as a share of all statement lines, in percent.
    pub fn percent(&self) -> f64 {
        if self.lines == 0 {
            0.0
        } else {
            self.duplicated_lines as f64 * 100.0 / self.lines as f64
        }
    }
}

/// A hash of the class's type and its copies' files and functions.
fn class_id(class: &CloneClass) -> String {
    let mut places: Vec<String> = class
        .copies
        .iter()
        .map(|c| format!("{}\0{}", c.file, c.function.as_deref().unwrap_or("")))
        .collect();
    places.sort();
    let kind = format!("{}{}", class.kind, class.clone_type);
    let mut parts: Vec<&[u8]> = vec![kind.as_bytes()];
    parts.extend(places.iter().map(|p| p.as_bytes()));
    format!("{:012x}", tokens::fnv(&parts) & 0xffff_ffff_ffff)
}

/// The project a file belongs to: the name of the nearest .csproj above it,
/// or the folder of the nearest package.json or Cargo.toml; `.` at the root.
fn project_of(
    root: &Path,
    file: &Path,
    found: &mut std::collections::HashMap<PathBuf, String>,
) -> String {
    let full = root.join(file);
    for dir in full.ancestors().skip(1).take_while(|d| d.starts_with(root)) {
        if let Some(name) = found.get(dir) {
            return name.clone();
        }
        if let Some(name) = project_in(root, dir) {
            found.insert(dir.to_path_buf(), name.clone());
            return name;
        }
    }
    ".".to_string()
}

fn project_in(root: &Path, dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut marker = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "csproj") {
            return path.file_stem().map(|s| s.to_string_lossy().into_owned());
        }
        marker |= path
            .file_name()
            .is_some_and(|n| n == "package.json" || n == "Cargo.toml");
    }
    let relative = dir
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    marker.then(|| {
        if relative.is_empty() {
            ".".to_string()
        } else {
            relative
        }
    })
}

/// Scans the C#, TypeScript/JavaScript and Rust files under `paths`
/// (honoring .gitignore); test code is left out unless `tests` is set.
/// Paths in the report are relative to `root` where they can be.
pub fn scan(paths: &[PathBuf], root: &Path, tests: bool, settings: Settings) -> Report {
    let paths: Vec<PathBuf> = paths
        .iter()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .collect();
    let root = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let root = root.as_path();
    let files: Vec<PathBuf> = source_files(&paths)
        .into_iter()
        .filter(|p| tests || !is_test_code(p))
        .collect();
    scan_files(&files, root, tests, settings)
}

/// Scans exactly `files`; with `tests`, test modules inside them too.
pub fn scan_files(files: &[PathBuf], root: &Path, tests: bool, settings: Settings) -> Report {
    let parsed: Vec<(tokens::Source, Vec<tokens::Block>)> = files
        .par_iter()
        .enumerate()
        .filter_map(|(index, path)| tokens::blocks(path, index, tests))
        .collect();
    // Number the files again: some did not parse.
    let mut sources = Vec::new();
    let mut blocks = Vec::new();
    for (index, (source, file_blocks)) in parsed.into_iter().enumerate() {
        sources.push(source);
        blocks.extend(file_blocks.into_iter().map(|mut b| {
            b.file = index;
            b
        }));
    }
    let lines = statement_lines(&blocks);
    let display = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let classes = find::clone_classes(&blocks, &sources, &display, settings);
    let used: std::collections::BTreeSet<&str> = classes
        .iter()
        .flat_map(|c| c.copies.iter().map(|copy| copy.file.as_str()))
        .collect();
    let file_lines = sources
        .iter()
        .map(|s| (display(&s.path), s.text.lines().count()))
        .filter(|(file, _)| used.contains(file.as_str()))
        .collect();
    let duplicated_lines = duplicated(&classes);
    let mut report = Report {
        scope: None,
        files: sources.len(),
        lines,
        duplicated_lines,
        classes,
        file_lines,
        projects: Default::default(),
        suppressed: 0,
        decisions: Default::default(),
    };
    report.index(root);
    report
}

/// Lines covered by the outermost blocks' statements.
fn statement_lines(blocks: &[tokens::Block]) -> usize {
    let mut by_file: std::collections::HashMap<usize, std::collections::BTreeSet<usize>> =
        std::collections::HashMap::new();
    for block in blocks {
        let lines = by_file.entry(block.file).or_default();
        for s in &block.statements {
            lines.extend(s.start_line..=s.end_line);
        }
    }
    by_file.values().map(|l| l.len()).sum()
}

fn source_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in paths {
        if root.is_file() {
            found.push(root.clone());
            continue;
        }
        let walker = ignore::WalkBuilder::new(root).require_git(false).build();
        found.extend(
            walker
                .flatten()
                .map(|e| e.into_path())
                .filter(|p| p.is_file() && dog_food_core::Language::from_path(p).is_some())
                .filter(|p| !is_generated(p)),
        );
    }
    found.sort();
    found.dedup();
    found
}

/// Code nobody writes by hand.
fn is_generated(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.ends_with(".d.ts")
        || name.ends_with(".g.cs")
        || name.ends_with(".designer.cs")
        || path
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("Migrations"))
}

/// Tests, by name or folder: `*.spec.ts`, `*.test.ts`, `*Tests.cs`,
/// `tests/`, `__tests__/`, `e2e/`, `*.Tests/`.
pub fn is_test_code(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_lowercase();
    lower.contains(".spec.")
        || lower.contains(".test.")
        || name.ends_with("Tests.cs")
        || name.ends_with("Test.cs")
        || path.components().any(|c| {
            let part = c.as_os_str().to_string_lossy();
            let part_lower = part.to_lowercase();
            part_lower == "tests"
                || part_lower == "__tests__"
                || part_lower == "test"
                || part_lower == "e2e"
                || part.ends_with(".Tests")
        })
}
