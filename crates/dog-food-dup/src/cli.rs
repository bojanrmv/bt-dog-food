//! The command line of `dgf-dup`, and of `dgf duplication`, which takes the
//! same arguments.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use dog_food_core::progress;

use crate::changes::Changes;
use crate::{report, risk, scan, semantic, Report, Settings};

#[derive(Parser)]
#[command(
    name = "dgf-dup",
    version,
    about = "Finds code that is the same apart from names and values, and says how to unite it"
)]
pub struct Args {
    /// Files or folders to scan (honoring .gitignore)
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
    /// Write the report to this file instead of the terminal
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
    /// Fewest tokens a copy has
    #[arg(long, default_value_t = Settings::default().min_tokens)]
    min_tokens: usize,
    /// Fewest lines a copy spans
    #[arg(long, default_value_t = Settings::default().min_lines)]
    min_lines: usize,
    /// Scan test code too
    #[arg(long)]
    tests: bool,
    /// How many clone classes to show; 0 shows all
    #[arg(long, default_value_t = 20)]
    top: usize,
    /// Exit with code 1 when duplicated lines are above this share of all lines, in percent
    #[arg(long, value_name = "PERCENT")]
    fail_above: Option<f64>,
    /// Only findings with a copy in what this branch changed (since its merge
    /// base with --base, uncommitted and new files included); the whole
    /// repository is scanned, so copies of existing code are found
    #[arg(long)]
    changes: bool,
    /// With --changes: the branch to compare with [default: main, else master]
    #[arg(long, requires = "changes")]
    base: Option<String>,
    /// With --changes: exit with code 1 when the branch added any copy
    #[arg(long, requires = "changes")]
    fail_on_copies: bool,
    /// Require the C# engine dgf-dup-cs, and fail if it cannot run. It finds
    /// C# methods that do the same thing written differently (a loop and its
    /// LINQ, `if` and `?:`) and repeated queries. Without this flag it runs
    /// anyway when there is C# to scan, except with --changes, and a report
    /// note says so if it cannot
    #[arg(long)]
    semantic: bool,
    /// Do not run the C# engine
    #[arg(long, conflicts_with_all = ["semantic", "semantic_engine", "confirm"])]
    no_semantic: bool,
    /// The C# engine to run: an executable, or its .dll; implies --semantic
    /// [default: DGF_DUP_CS, else dgf-dup-cs on PATH]
    #[arg(long, value_name = "PATH")]
    semantic_engine: Option<PathBuf>,
    /// Run each group of methods said to do the same thing on generated
    /// inputs, and leave out any an input tells apart (methods with effects,
    /// awaits or database reads are not run); implies --semantic
    #[arg(long)]
    confirm: bool,
    /// Review decisions saved from the HTML report: groups marked intentional
    /// or a false positive are left out, so gates only see unreviewed ones
    #[arg(long, value_name = "FILE")]
    decisions: Option<PathBuf>,
    /// Coverage reports (Cobertura, OpenCover or LCOV files, or folders
    /// holding them): each group gets the CRAP score of its riskiest copy
    #[arg(long, value_name = "PATH", num_args = 1..)]
    coverage: Vec<PathBuf>,
    /// How to order the groups: by lines saved, or by risk (the CRAP score
    /// with --coverage, else the complexity, of the riskiest copy's function)
    /// [default: risk with --coverage, else saves]
    #[arg(long, value_enum)]
    rank: Option<Rank>,
}

/// Whether the C# engine runs, and what it means if it cannot.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Engine {
    Off,
    /// It runs by default: if it cannot, the report says so and carries on.
    WhenAvailable,
    /// It was asked for: if it cannot, the run fails.
    Required,
}

/// `has_csharp` is only asked when the answer depends on it: it walks the files.
fn engine_mode(args: &Args, has_csharp: impl FnOnce() -> bool) -> Engine {
    if args.no_semantic {
        return Engine::Off;
    }
    if args.asks_for_engine() {
        return Engine::Required;
    }
    // --changes scans the merge base too, so the engine would run twice: a
    // PR gate stays fast unless it asks for the engine.
    if !args.changes && has_csharp() {
        Engine::WhenAvailable
    } else {
        Engine::Off
    }
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Rank {
    Saves,
    Risk,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    /// Readable text
    Table,
    /// Compact text for AI assistants
    Agent,
    /// Everything, for other tools
    Json,
    /// A self-contained page: the files with their copies, and each group
    Html,
}

impl Args {
    fn asks_for_engine(&self) -> bool {
        self.semantic || self.confirm || self.semantic_engine.is_some()
    }

    pub fn has_coverage(&self) -> bool {
        !self.coverage.is_empty()
    }

    /// Coverage reports to read, as `--coverage` gives them.
    pub fn set_coverage(&mut self, paths: Vec<PathBuf>) {
        self.coverage = paths;
    }
}

/// `dgf-dup` itself: parses the command line and runs.
pub fn main() -> ExitCode {
    run_args(&Args::parse())
}

/// Runs with parsed arguments: what `dgf duplication` calls.
pub fn run_args(args: &Args) -> ExitCode {
    match run(args) {
        Ok(code) => code,
        Err((code, err)) => {
            progress::finish();
            eprintln!("error: {err}");
            ExitCode::from(code)
        }
    }
}

/// Scans, reports and says how it went; an error with its exit code.
fn run(args: &Args) -> Result<ExitCode, (u8, String)> {
    let settings = Settings {
        min_tokens: args.min_tokens,
        min_lines: args.min_lines,
    };
    let changes = changes(args)?;
    let (paths, root) = scope(args, changes.as_ref());
    // The C# engine runs in its own process, alongside the syntactic scan.
    let mode = engine_mode(args, || crate::has_csharp(&paths, args.tests));
    let mut notes = Vec::new();
    let engine = tolerate(mode, start_semantic(args, mode, &paths, &root), &mut notes)?.flatten();
    progress::step("finding copies");
    let mut found = scan(&paths, &root, args.tests, settings);
    found.notes = notes;
    progress::done(format!(
        "{} groups in {} files",
        found.classes.len(),
        found.files
    ));
    semantic_step(args, mode, &mut found, engine, (&paths, &root))?;
    if let Some(path) = &args.decisions {
        found.apply(crate::decisions::load(path).map_err(|e| (2, e))?);
    }
    if let Some(changes) = &changes {
        progress::step(format!(
            "the same scan of the merge base {}, to leave out what was there",
            changes.commit
        ));
        let at_base = at_base(changes, args, mode, &paths, settings)?;
        progress::done(format!("{} groups", at_base.len()));
        found.keep_changed(changes, Some(&at_base));
    }
    assess(&mut found, args, &root)?;
    progress::finish();
    write(&render(&found, args), args.output.as_deref())?;
    // Repeated queries are advice: a gate fails on copied code only.
    let copied = args.fail_on_copies && found.classes.iter().any(|c| c.kind != "query");
    let too_much = args.fail_above.is_some_and(|limit| found.percent() > limit);
    Ok(if copied || too_much {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// The ids of the groups the merge base had, scanned the same way in a copy
/// of its files: a group the branch did not add is not its finding.
fn at_base(
    changes: &Changes,
    args: &Args,
    mode: Engine,
    paths: &[PathBuf],
    settings: Settings,
) -> Result<std::collections::BTreeSet<String>, (u8, String)> {
    let dir = std::env::temp_dir().join(format!("dgf-dup-base-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| (3, format!("{}: {e}", dir.display())))?;
    let scanned = (|| {
        changes.export_base(&dir).map_err(|e| (2, e))?;
        let base_paths: Vec<PathBuf> = paths
            .iter()
            .filter_map(|p| {
                let p = std::path::absolute(p).ok()?;
                Some(dir.join(p.strip_prefix(&changes.root).ok()?))
            })
            .collect();
        let engine = start_semantic(args, mode, &base_paths, &dir).map_err(|e| (2, e))?;
        let mut found = scan(&base_paths, &dir, args.tests, settings);
        add_semantic(&mut found, engine, &dir).map_err(|e| (2, e))?;
        Ok(found.classes.into_iter().map(|c| c.id).collect())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    scanned
}

/// Waits for the C# engine, as a step of its own: it usually takes longest.
fn semantic_step(
    args: &Args,
    mode: Engine,
    found: &mut Report,
    engine: Option<std::process::Child>,
    (paths, root): (&[PathBuf], &std::path::Path),
) -> Result<(), (u8, String)> {
    if engine.is_none() {
        return Ok(());
    }
    progress::step(if args.confirm {
        "C# engine: the same behaviour written differently, then running it to confirm"
    } else {
        "C# engine: the same behaviour written differently, repeated queries"
    });
    let before = found.classes.len();
    let added = add_semantic(found, engine, root);
    if tolerate(mode, added, &mut found.notes)?.is_none() {
        progress::done("skipped");
        return Ok(());
    }
    found
        .notes
        .extend(semantic::restore_note(paths, args.tests));
    progress::done(format!(
        "{} more groups",
        found.classes.len().saturating_sub(before)
    ));
    Ok(())
}

/// The C# engine's classes in the report, once it has the syntactic C#
/// copies' signatures too.
fn add_semantic(
    found: &mut Report,
    engine: Option<std::process::Child>,
    root: &std::path::Path,
) -> Result<(), String> {
    let Some(engine) = engine else {
        return Ok(());
    };
    let classes = semantic::finish_with(engine, found)?;
    semantic::merge(found, classes, root);
    Ok(())
}

/// What the engine's `result` means in this `mode`: an error when it was
/// asked for; otherwise a note in the report, and nothing to go on with.
fn tolerate<T>(
    mode: Engine,
    result: Result<T, String>,
    notes: &mut Vec<String>,
) -> Result<Option<T>, (u8, String)> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(err) if mode == Engine::Required => Err((2, err)),
        Err(err) => {
            notes.push(semantic::skipped_note(&err));
            Ok(None)
        }
    }
}

/// Each group's risk; ordered by it with `--rank risk` (the default with coverage).
fn assess(found: &mut Report, args: &Args, root: &std::path::Path) -> Result<(), (u8, String)> {
    let coverage = if args.coverage.is_empty() {
        None
    } else {
        Some(dog_food_core::coverage::Coverage::load(&args.coverage).map_err(|e| (2, e))?)
    };
    risk::assess(found, root, coverage.as_ref());
    let rank = args.rank.unwrap_or(if coverage.is_some() {
        Rank::Risk
    } else {
        Rank::Saves
    });
    if rank == Rank::Risk {
        risk::rank(found);
    }
    Ok(())
}

/// The C# engine started on the scanned paths, unless `mode` has it off.
fn start_semantic(
    args: &Args,
    mode: Engine,
    paths: &[PathBuf],
    root: &std::path::Path,
) -> Result<Option<std::process::Child>, String> {
    if mode == Engine::Off {
        return Ok(None);
    }
    let engine = semantic::command(args.semantic_engine.as_deref());
    let ask = semantic::Ask {
        tests: args.tests,
        confirm: args.confirm,
        regions: true,
    };
    semantic::start(engine, paths, root, ask).map(Some)
}

fn changes(args: &Args) -> Result<Option<Changes>, (u8, String)> {
    if !args.changes {
        return Ok(None);
    }
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Changes::since(&here, args.base.as_deref())
        .map(Some)
        .map_err(|err| (2, err))
}

/// What to scan and what paths are relative to. With --changes and no
/// paths, the whole repository: a new copy is usually a copy of code the
/// branch did not touch.
fn scope(args: &Args, changes: Option<&Changes>) -> (Vec<PathBuf>, PathBuf) {
    match changes {
        Some(c) if args.paths == [PathBuf::from(".")] => (vec![c.root.clone()], c.root.clone()),
        Some(c) => (args.paths.clone(), c.root.clone()),
        None => (
            args.paths.clone(),
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ),
    }
}

fn render(found: &Report, args: &Args) -> String {
    match args.format {
        Format::Table => report::table(found, args.top),
        Format::Agent => report::agent(found, args.top),
        Format::Json => report::json(found),
        Format::Html => report::html(found),
    }
}

fn write(text: &str, output: Option<&std::path::Path>) -> Result<(), (u8, String)> {
    let Some(path) = output else {
        use std::io::Write as _;
        let _ = std::io::stdout().write_all(text.as_bytes());
        return Ok(());
    };
    std::fs::write(path, text)
        .map_err(|err| (3, format!("could not write {}: {err}", path.display())))?;
    eprintln!("Wrote the report to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(command: &[&str], has_csharp: bool) -> Engine {
        let line = std::iter::once("dgf-dup").chain(command.iter().copied());
        engine_mode(&Args::parse_from(line), || has_csharp)
    }

    #[test]
    fn the_engine_runs_when_there_is_csharp_and_may_be_missing() {
        assert_eq!(mode(&[], true), Engine::WhenAvailable);
        assert_eq!(mode(&[], false), Engine::Off);
    }

    #[test]
    fn asking_for_the_engine_in_any_way_requires_it() {
        for command in [
            &["--semantic"][..],
            &["--confirm"],
            &["--semantic-engine", "x.dll"],
            &["--semantic", "--changes"],
        ] {
            assert_eq!(mode(command, false), Engine::Required, "{command:?}");
        }
    }

    #[test]
    fn a_branch_check_and_no_semantic_leave_the_engine_off() {
        assert_eq!(mode(&["--changes"], true), Engine::Off);
        assert_eq!(mode(&["--no-semantic"], true), Engine::Off);
    }

    #[test]
    fn no_semantic_cannot_be_combined_with_asking_for_the_engine() {
        for flag in ["--semantic", "--confirm"] {
            assert!(Args::try_parse_from(["dgf-dup", "--no-semantic", flag]).is_err());
        }
    }

    #[test]
    fn a_failure_is_an_error_only_when_the_engine_was_asked_for() {
        let failed = || Err::<(), _>("no engine".to_string());
        let mut notes = Vec::new();
        assert!(tolerate(Engine::Required, failed(), &mut notes).is_err());
        assert!(notes.is_empty());
        assert_eq!(
            tolerate(Engine::WhenAvailable, failed(), &mut notes),
            Ok(None)
        );
        assert!(notes[0].contains("no engine"), "{notes:?}");
    }
}
