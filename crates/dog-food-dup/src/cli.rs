//! The command line of `dgf-dup`, and of `dgf duplication`, which takes the
//! same arguments.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

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
    /// Also find C# methods that do the same thing written differently (a
    /// loop and its LINQ, `if` and `?:`), with the C# engine dgf-dup-cs
    #[arg(long)]
    semantic: bool,
    /// The C# engine to run: an executable, or its .dll [default: DGF_DUP_CS, else dgf-dup-cs on PATH]
    #[arg(long, value_name = "PATH", requires = "semantic")]
    semantic_engine: Option<PathBuf>,
    /// With --semantic: run each group of methods said to do the same thing
    /// on generated inputs, and leave out any an input tells apart (methods
    /// with effects, awaits or database reads are not run)
    #[arg(long, requires = "semantic")]
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
    let engine = start_semantic(args, &paths, &root)?;
    let mut found = scan(&paths, &root, args.tests, settings);
    add_semantic(&mut found, engine, &root)?;
    if let Some(path) = &args.decisions {
        found.apply(crate::decisions::load(path).map_err(|e| (2, e))?);
    }
    if let Some(changes) = &changes {
        found.keep_changed(changes);
    }
    assess(&mut found, args, &root)?;
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

/// The C# engine's classes in the report, once it has the syntactic C#
/// copies' signatures too.
fn add_semantic(
    found: &mut Report,
    engine: Option<std::process::Child>,
    root: &std::path::Path,
) -> Result<(), (u8, String)> {
    let Some(engine) = engine else {
        return Ok(());
    };
    let classes = semantic::finish_with(engine, found).map_err(|e| (2, e))?;
    semantic::merge(found, classes, root);
    Ok(())
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

/// With --semantic, the C# engine started on the scanned paths.
fn start_semantic(
    args: &Args,
    paths: &[PathBuf],
    root: &std::path::Path,
) -> Result<Option<std::process::Child>, (u8, String)> {
    if !args.semantic {
        return Ok(None);
    }
    let engine = semantic::command(args.semantic_engine.as_deref());
    let ask = semantic::Ask {
        tests: args.tests,
        confirm: args.confirm,
        regions: true,
    };
    semantic::start(engine, paths, root, ask)
        .map(Some)
        .map_err(|e| (2, e))
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
