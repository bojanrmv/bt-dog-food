//! `dgf coverage`: line and branch coverage per file, read from the
//! coverage reports: for the files a branch changed, or for every source
//! file the reports cover.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dog_food_core::coverage::{branch_counts, Branches, Lines};
use rayon::prelude::*;

use crate::coverage::Coverage;
use crate::lookup::{changed, range, shown, CommonArgs, DEFAULT_LINES};
use crate::mutate::is_test_code;
use crate::{discover, git, usage, Failure, Settings, EXIT_FAIL};

/// Line ranges listed for one file or function before the rest are counted.
const RANGES: usize = 12;

#[derive(clap::Args)]
pub struct Args {
    /// Coverage reports (Cobertura, OpenCover or LCOV) or folders with reports [default: `[coverage] paths` in dgf.toml]
    reports: Vec<PathBuf>,
    /// Only the files this branch and its uncommitted edits changed
    #[arg(long)]
    changes: bool,
    /// Base for --changes [default: `base` in dgf.toml, else main or master]
    #[arg(long, value_name = "REF", requires = "changes")]
    base: Option<String>,
    /// Exit with code 1 when a file is below this percentage on lines or on branches
    #[arg(long, value_name = "PERCENT")]
    floor: Option<f64>,
    /// Also a row for every function of each file
    #[arg(long)]
    functions: bool,
    #[command(flatten)]
    common: CommonArgs,
}

/// How many of something ran.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Count {
    covered: usize,
    total: usize,
}

impl Count {
    fn percent(self) -> Option<f64> {
        (self.total > 0).then(|| 100.0 * self.covered as f64 / self.total as f64)
    }

    fn below(self, floor: f64) -> bool {
        self.percent().is_some_and(|percent| percent < floor)
    }

    fn shown(self) -> String {
        self.percent()
            .map_or_else(|| "-".to_string(), |percent| format!("{percent:.1}"))
    }

    fn add(&mut self, other: Count) {
        self.covered += other.covered;
        self.total += other.total;
    }
}

/// The coverage of some lines of a file.
#[derive(Debug, Default, PartialEq, Eq)]
struct Part {
    lines: Count,
    branches: Count,
    /// Runs of lines that never ran.
    not_run: Vec<(usize, usize)>,
    /// Lines that ran with only some of their branches: line, taken, all.
    partly: Vec<(usize, usize, usize)>,
}

impl Part {
    /// The lower of the line and the branch percentage; 100 with neither.
    fn worst(&self) -> f64 {
        let lines = self.lines.percent().unwrap_or(100.0);
        lines.min(self.branches.percent().unwrap_or(100.0))
    }
}

struct FunctionRow {
    name: String,
    start: usize,
    end: usize,
    part: Part,
}

struct FileRow {
    path: String,
    /// `None` when no report has the file.
    part: Option<Part>,
    /// The file has functions, so a report should have it.
    has_code: bool,
    functions: Vec<FunctionRow>,
}

impl FileRow {
    fn below(&self, floor: f64) -> bool {
        match &self.part {
            Some(part) => part.lines.below(floor) || part.branches.below(floor),
            None => self.has_code,
        }
    }

    /// The measured files first, the worst covered on lines or on branches
    /// at the top; then what no report has.
    fn order(&self) -> (u8, u64, &str) {
        let rank = match &self.part {
            Some(part) => (0, (part.worst() * 100.0) as u64),
            None if self.has_code => (1, 0),
            None => (2, 0),
        };
        (rank.0, rank.1, &self.path)
    }
}

pub fn run(args: Args) -> Result<ExitCode, Failure> {
    let settings = args.common.settings()?;
    let coverage = crate::load_coverage(&args.reports, &settings)?.ok_or_else(|| {
        usage(
            "no coverage report: name a report or a folder, or set [coverage] paths in dgf.toml"
                .to_string(),
        )
    })?;
    let root = git::root(&settings.cwd).unwrap_or_else(|_| settings.cwd.clone());
    let (files, mut text) = wanted(&args, &settings, &coverage, &root)?;
    let mut rows: Vec<FileRow> = files
        .par_iter()
        .map(|file| file_row(file, &coverage, &root, &settings.cwd, args.functions))
        .collect();
    rows.sort_by(|a, b| a.order().cmp(&b.order()));
    text.push_str(&table(&rows, args.common.budget(DEFAULT_LINES), args.floor));
    text.push_str(&total(&rows));
    let below = args
        .floor
        .map(|floor| (floor, rows.iter().filter(|row| row.below(floor)).count()));
    if let Some((floor, below)) = below {
        text.push_str(&verdict(floor, below, rows.len()));
    }
    crate::progress::finish();
    say!("{text}");
    let failed = below.is_some_and(|(_, below)| below > 0);
    Ok(ExitCode::from(if failed { EXIT_FAIL } else { 0 }))
}

/// The files to report, and the first lines of the report: warnings, then
/// what is covered and from how many reports.
fn wanted(
    args: &Args,
    settings: &Settings,
    coverage: &Coverage,
    root: &Path,
) -> Result<(Vec<PathBuf>, String), Failure> {
    let reports = coverage.sources.len();
    if !args.changes {
        let all = discover::discover(
            std::slice::from_ref(&settings.cwd),
            settings.excludes.clone(),
        );
        let covered: Vec<PathBuf> = all
            .into_iter()
            .filter(|file| !is_test_code(file) && in_reports(coverage, file, root).is_some())
            .collect();
        let heading = format!(
            "coverage: {} source files in the reports, {reports} reports\n",
            covered.len()
        );
        return Ok((covered, heading));
    }
    let found = changed(settings, args.base.clone())?;
    let mut heading: String = found
        .warnings
        .iter()
        .map(|warning| format!("warning: {warning}\n"))
        .collect();
    // Reports measure the code under test, not the tests.
    let (tests, code): (Vec<PathBuf>, Vec<PathBuf>) =
        found.files.into_iter().partition(|file| is_test_code(file));
    heading.push_str(&format!(
        "coverage: {} changed source files against {} ({} changed test files left out), {reports} reports\n",
        code.len(),
        found.against,
        tests.len()
    ));
    Ok((code, heading))
}

/// One row per file, the worst covered first, up to `budget` files.
fn table(rows: &[FileRow], budget: usize, floor: Option<f64>) -> String {
    let mut text = String::new();
    if !rows.is_empty() {
        text.push_str("  line%  branch%  file\n");
    }
    for row in rows.iter().take(budget) {
        text.push_str(&render(row, floor));
    }
    if rows.len() > budget {
        let cut = &rows[budget..];
        let missing = cut.iter().filter(|row| row.part.is_none()).count();
        text.push_str(&format!(
            "... {} more files: {} better covered than these, {missing} not in the reports; raise --max-lines\n",
            cut.len(),
            cut.len() - missing
        ));
    }
    text
}

fn in_reports<'a>(
    coverage: &'a Coverage,
    file: &Path,
    root: &Path,
) -> Option<(&'a Lines, Option<&'a Branches>)> {
    let full = discover::absolute(file);
    let relative = full.strip_prefix(root).unwrap_or(Path::new(""));
    coverage.of(&full, relative)
}

fn file_row(file: &Path, coverage: &Coverage, root: &Path, cwd: &Path, split: bool) -> FileRow {
    let found = in_reports(coverage, file, root);
    let needs_functions = split || found.is_none();
    let functions = if needs_functions {
        dog_food_core::analyze_file(file)
            .map(|report| report.functions)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let per_function = match found {
        Some((lines, branches)) if split => functions
            .iter()
            .filter(|f| !matches!(f.kind, "lambda" | "arrow" | "closure"))
            .map(|f| FunctionRow {
                name: f.name.clone(),
                start: f.start_line,
                end: f.end_line,
                part: part(lines, branches, f.start_line, f.end_line),
            })
            .filter(|row| row.part.lines.total > 0)
            .collect(),
        _ => Vec::new(),
    };
    FileRow {
        path: shown(file, cwd),
        part: found.map(|(lines, branches)| part(lines, branches, 1, usize::MAX)),
        has_code: found.is_some() || !functions.is_empty(),
        functions: per_function,
    }
}

/// The coverage of lines `start..=end`.
fn part(lines: &Lines, branches: Option<&Branches>, start: usize, end: usize) -> Part {
    let mut measured: Vec<(usize, u64)> = lines
        .iter()
        .filter(|(line, _)| (start..=end).contains(*line))
        .map(|(line, hits)| (*line, *hits))
        .collect();
    measured.sort();
    let mut not_run: Vec<(usize, usize)> = Vec::new();
    let mut open = false;
    for (line, hits) in &measured {
        let missed = *hits == 0;
        match not_run.last_mut() {
            // Still in a run: no line that ran lies between.
            Some(last) if missed && open => last.1 = *line,
            _ if missed => not_run.push((*line, *line)),
            _ => {}
        }
        open = missed;
    }
    let ran = |line: &usize| lines.get(line).is_some_and(|hits| *hits > 0);
    let mut partly: Vec<(usize, usize, usize)> = branches
        .into_iter()
        .flatten()
        .filter(|(line, _)| (start..=end).contains(*line) && ran(line))
        .map(|(line, hits)| (*line, hits.iter().filter(|h| **h > 0).count(), hits.len()))
        .filter(|(_, taken, all)| taken < all)
        .collect();
    partly.sort();
    let (taken, all) = branches.map_or((0, 0), |b| branch_counts(b, start, end));
    Part {
        lines: Count {
            covered: measured.iter().filter(|(_, hits)| *hits > 0).count(),
            total: measured.len(),
        },
        branches: Count {
            covered: taken,
            total: all,
        },
        not_run,
        partly,
    }
}

/// Up to [`RANGES`] items, then how many more there are.
fn listed(items: Vec<String>) -> String {
    let more = items.len().saturating_sub(RANGES);
    let mut text = items
        .into_iter()
        .take(RANGES)
        .collect::<Vec<_>>()
        .join(", ");
    if more > 0 {
        text.push_str(&format!(", ... {more} more"));
    }
    text
}

/// `not run: 4-9, 12` and `branches partly taken: 20 (1/2)`, each on its
/// own line under the row they belong to.
fn details(part: &Part, indent: &str) -> String {
    let mut text = String::new();
    if !part.not_run.is_empty() {
        let ranges = part.not_run.iter().map(|(a, b)| range(*a, *b)).collect();
        text.push_str(&format!("{indent}not run: {}\n", listed(ranges)));
    }
    if !part.partly.is_empty() {
        let lines = part
            .partly
            .iter()
            .map(|(line, taken, all)| format!("{line} ({taken}/{all})"))
            .collect();
        text.push_str(&format!(
            "{indent}branches partly taken: {}\n",
            listed(lines)
        ));
    }
    text
}

fn render(row: &FileRow, floor: Option<f64>) -> String {
    let below = floor.filter(|floor| row.below(*floor));
    let mark = below.map_or_else(String::new, |floor| format!("  BELOW {floor}"));
    let Some(part) = &row.part else {
        let why = if row.has_code {
            "not in the reports"
        } else {
            "not in the reports, no functions"
        };
        return format!("{:>7}  {:>7}  {}  {why}{mark}\n", "-", "-", row.path);
    };
    let mut text = format!(
        "{:>7}  {:>7}  {}{mark}\n",
        part.lines.shown(),
        part.branches.shown(),
        row.path
    );
    let indent = " ".repeat(20);
    if row.functions.is_empty() {
        text.push_str(&details(part, &indent));
    }
    for function in &row.functions {
        text.push_str(&format!(
            "{:>7}  {:>7}    {} ({})\n",
            function.part.lines.shown(),
            function.part.branches.shown(),
            function.name,
            range(function.start, function.end)
        ));
        text.push_str(&details(&function.part, &indent));
    }
    text
}

fn total(rows: &[FileRow]) -> String {
    let mut lines = Count::default();
    let mut branches = Count::default();
    for part in rows.iter().filter_map(|row| row.part.as_ref()) {
        lines.add(part.lines);
        branches.add(part.branches);
    }
    let missing = rows
        .iter()
        .filter(|row| row.part.is_none() && row.has_code)
        .count();
    let mut text = if lines.total == 0 {
        "total: no measured lines".to_string()
    } else {
        format!(
            "total: {}% of lines ({}/{}), {}% of branches ({}/{})",
            lines.shown(),
            lines.covered,
            lines.total,
            branches.shown(),
            branches.covered,
            branches.total
        )
    };
    if missing > 0 {
        text.push_str(&format!(
            "; {missing} files with functions are not in the reports (no test ran them, or their project was not measured) and are not counted"
        ));
    }
    text.push('\n');
    text
}

fn verdict(floor: f64, below: usize, files: usize) -> String {
    if files == 0 {
        format!("floor {floor}: no files to check\n")
    } else if below == 0 {
        format!("floor {floor}: pass, every file is at or above it on lines and branches\n")
    } else {
        format!("floor {floor}: fail, {below} files are below it on lines or branches\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines() -> Lines {
        [(3, 2), (4, 0), (6, 0), (7, 1), (9, 0), (12, 5)]
            .into_iter()
            .collect()
    }

    fn branches() -> Branches {
        [(3, vec![2, 0]), (4, vec![0, 0]), (12, vec![1, 4])]
            .into_iter()
            .collect()
    }

    #[test]
    fn lines_that_never_ran_are_joined_across_unmeasured_lines() {
        let whole = part(&lines(), Some(&branches()), 1, usize::MAX);
        assert_eq!(
            whole.lines,
            Count {
                covered: 3,
                total: 6
            }
        );
        // 5 is not measured, so 4 and 6 are one run; 7 ran and ends it.
        assert_eq!(whole.not_run, [(4, 6), (9, 9)]);
        assert_eq!(
            whole.branches,
            Count {
                covered: 3,
                total: 6
            }
        );
        // Line 4 never ran: it is in `not_run`, not here.
        assert_eq!(whole.partly, [(3, 1, 2)]);
        let first = part(&lines(), Some(&branches()), 1, 4);
        assert_eq!(first.lines.shown(), "50.0");
        assert_eq!(first.not_run, [(4, 4)]);
        let none = part(&lines(), None, 20, 30);
        assert_eq!(none.lines.percent(), None);
        assert_eq!(none.branches.shown(), "-");
    }

    fn row(path: &str, part: Option<Part>, has_code: bool) -> FileRow {
        FileRow {
            path: path.to_string(),
            part,
            has_code,
            functions: Vec::new(),
        }
    }

    #[test]
    fn a_file_is_below_the_floor_on_lines_or_on_branches() {
        let half = row(
            "a.cs",
            Some(part(&lines(), Some(&branches()), 1, usize::MAX)),
            true,
        );
        assert!(half.below(90.0));
        assert!(!half.below(50.0));
        let all_lines: Lines = [(1, 1), (2, 1)].into_iter().collect();
        let one_branch: Branches = [(1, vec![1, 0])].into_iter().collect();
        let branchy = row(
            "b.cs",
            Some(part(&all_lines, Some(&one_branch), 1, 9)),
            true,
        );
        assert!(branchy.below(90.0));
        let no_branches = row("c.cs", Some(part(&all_lines, None, 1, 9)), true);
        assert!(!no_branches.below(90.0));
        // Code no report has counts as not covered; a file without code does not.
        assert!(row("d.cs", None, true).below(1.0));
        assert!(!row("IThing.cs", None, false).below(90.0));
    }

    #[test]
    fn rows_show_what_did_not_run_and_the_floor() {
        let half = row(
            "src/a.cs",
            Some(part(&lines(), Some(&branches()), 1, usize::MAX)),
            true,
        );
        assert_eq!(
            render(&half, Some(90.0)),
            "   50.0     50.0  src/a.cs  BELOW 90\n                    not run: 4-6, 9\n                    branches partly taken: 3 (1/2)\n"
        );
        assert_eq!(
            render(&row("src/d.cs", None, true), None),
            "      -        -  src/d.cs  not in the reports\n"
        );
        let rows = [half, row("src/d.cs", None, true)];
        assert_eq!(
            total(&rows),
            "total: 50.0% of lines (3/6), 50.0% of branches (3/6); 1 files with functions are not in the reports (no test ran them, or their project was not measured) and are not counted\n"
        );
        assert!(verdict(90.0, 2, 5).starts_with("floor 90: fail, 2 files"));
        assert!(verdict(90.0, 0, 5).starts_with("floor 90: pass"));
        assert_eq!(verdict(90.0, 0, 0), "floor 90: no files to check\n");
        assert_eq!(
            total(&[row("src/d.cs", None, true)]),
            "total: no measured lines; 1 files with functions are not in the reports (no test ran them, or their project was not measured) and are not counted\n"
        );
    }

    #[test]
    fn the_worst_covered_file_comes_first_and_what_no_report_has_last() {
        let full: Lines = [(1, 1)].into_iter().collect();
        // Every line of b.cs ran, but one of its two branches did not.
        let half: Branches = [(1, vec![1, 0])].into_iter().collect();
        let mut rows = [
            row("b.cs", Some(part(&full, Some(&half), 1, 9)), true),
            row("z.cs", Some(part(&full, None, 1, 9)), true),
            row("i.cs", None, false),
            row("m.cs", Some(part(&lines(), None, 1, 99)), true),
            row("a.cs", None, true),
        ];
        rows.sort_by(|a, b| a.order().cmp(&b.order()));
        let order: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(order, ["b.cs", "m.cs", "z.cs", "a.cs", "i.cs"]);
        let cut = table(&rows, 2, None);
        assert!(
            cut.ends_with("... 3 more files: 1 better covered than these, 2 not in the reports; raise --max-lines\n"),
            "{cut}"
        );
    }

    #[test]
    fn long_lists_are_cut() {
        let items = (1..=15).map(|n| n.to_string()).collect();
        assert!(listed(items).ends_with("11, 12, ... 3 more"));
    }
}
