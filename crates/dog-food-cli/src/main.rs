mod baseline;
mod changes;
mod config;
mod coverage;
mod discover;
mod git;
mod history;
mod limits;
mod mutate;
mod mutation;
mod report;
mod setup;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};
use dog_food_core::FileReport;
use rayon::prelude::*;

use baseline::Baseline;
use changes::{FailOn, Scope};
use config::Config;
use coverage::Coverage;
use discover::{Excludes, DEFAULT_EXCLUDES};
use limits::{Limits, Status};
use report::{Format, Row, Run};

#[derive(Parser)]
#[command(
    name = "dgf",
    version,
    about = "Per-function cyclomatic and cognitive complexity for C#, TypeScript and Rust"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Analyze files or directories and list functions by complexity
    Analyze(AnalyzeArgs),
    /// Compare your branch and uncommitted edits with a base branch
    Changes(ChangesArgs),
    /// Overview of the runs saved with `analyze --save-history`
    History(HistoryArgs),
    /// Accept today's functions above the limits, so only new problems fail
    Baseline(BaselineArgs),
    /// Mutation testing: choose functions, list their mutants, or run them against the tests
    Mutate(MutateArgs),
    /// Install or remove a git pre-commit hook that runs `dgf changes --staged`
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    /// Install the Claude Code skill that teaches the agent to use dgf
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Create a commented dgf.toml in the current folder
    Init,
}

#[derive(Subcommand)]
enum HookAction {
    /// Write .git/hooks/pre-commit
    Install {
        /// Replace a pre-commit hook that dog-food did not write
        #[arg(long)]
        force: bool,
    },
    /// Remove the hook written by `dgf hook install`
    Uninstall,
}

#[derive(Subcommand)]
enum SkillAction {
    /// Write .claude/skills/dog-food/SKILL.md in this repository
    Install {
        /// Install for every project of this user (~/.claude/skills) instead
        #[arg(long)]
        global: bool,
    },
    /// Print the skill
    Show,
}

#[derive(clap::Args)]
struct OutputArgs {
    /// Report format
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
    /// Write the report to this file instead of the terminal
    #[arg(long, short, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(clap::Args)]
struct LimitArgs {
    /// Flag functions above this complexity [default: 10, or `warn` in dgf.toml]
    #[arg(long, value_name = "N")]
    warn: Option<u32>,
    /// Exit with code 1 if a function is above this complexity [default: `fail` in dgf.toml]
    #[arg(long, value_name = "N", alias = "threshold")]
    fail: Option<u32>,
    /// Skip files or folders matching this .gitignore-style pattern (repeatable)
    #[arg(long = "exclude", value_name = "PATTERN")]
    excludes: Vec<String>,
    /// Ignore dgf.toml
    #[arg(long)]
    no_config: bool,
}

#[derive(clap::Args)]
struct AnalyzeArgs {
    /// Files or directories to analyze (.gitignore is respected)
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,
    /// Rows to show in the table, 0 = all [default: 20, or `top` in dgf.toml]
    #[arg(long)]
    top: Option<usize>,
    /// Coverage report (Cobertura, OpenCover or LCOV) or folder with reports; adds CRAP scores (repeatable)
    #[arg(long, value_name = "PATH")]
    coverage: Vec<PathBuf>,
    /// Do not apply .dgf/baseline.json
    #[arg(long)]
    no_baseline: bool,
    /// Also save a snapshot of this run for `dgf history`
    #[arg(long)]
    save_history: bool,
    #[command(flatten)]
    out: OutputArgs,
    #[command(flatten)]
    limits: LimitArgs,
}

#[derive(clap::Args)]
struct ChangesArgs {
    /// Branch or commit to compare with [default: `base` in dgf.toml, else main or master; HEAD with --staged]
    #[arg(long, value_name = "REF")]
    base: Option<String>,
    /// Compare only what is staged for the next commit (used by the pre-commit hook)
    #[arg(long)]
    staged: bool,
    /// Which functions in changed files to report [default: lines]
    #[arg(long, value_enum)]
    scope: Option<Scope>,
    /// Which reported functions can fail the run [default: new-or-worse]
    #[arg(long, value_enum)]
    fail_on: Option<FailOn>,
    #[command(flatten)]
    out: OutputArgs,
    #[command(flatten)]
    limits: LimitArgs,
}

#[derive(clap::Args)]
struct HistoryArgs {
    /// Folder with saved snapshots [default: `dir` in dgf.toml, else .dgf/history]
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Movers to list in the table and Markdown, 0 = all [default: 20]
    #[arg(long)]
    top: Option<usize>,
    /// Ignore dgf.toml
    #[arg(long)]
    no_config: bool,
    #[command(flatten)]
    out: OutputArgs,
}

#[derive(clap::Args)]
struct MutateArgs {
    /// Files or directories to mutate (not used with --changes)
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,
    /// Only functions that this branch and its uncommitted edits changed
    #[arg(long)]
    changes: bool,
    /// Base for --changes [default: `base` in dgf.toml, else main or master]
    #[arg(long, value_name = "REF")]
    base: Option<String>,
    /// Only functions with at least this cyclomatic complexity
    #[arg(long, value_name = "N", default_value_t = 1)]
    min_complexity: u32,
    /// List every mutant, not only the count per function
    #[arg(long)]
    show: bool,
    /// Build and test the mutants (C#, TypeScript, JavaScript): score per function, and every mutant no test noticed
    #[arg(long)]
    run: bool,
    /// Mutant test runs at the same time with --run [default: `jobs` in dgf.toml, else CPU cores - 1]
    #[arg(long, value_name = "N")]
    jobs: Option<usize>,
    /// With --run, exit with code 1 when the mutation score is below this percentage
    /// [default: `thresholds.break` in dgf.toml]
    #[arg(long, value_name = "PERCENT", alias = "break-at")]
    fail_under: Option<f64>,
    /// A test run counts as hanging after (startup + its tests' time without
    /// mutants) × this [default: 1.5]
    #[arg(long, value_name = "N")]
    timeout_factor: Option<f64>,
    /// ... plus this many milliseconds [default: 5000]
    #[arg(long, value_name = "MS")]
    timeout_ms: Option<u64>,
    /// Projects `dotnet build` builds at the same time (-m:N) [default: all cores]
    #[arg(long, value_name = "N")]
    build_jobs: Option<usize>,
    /// Run dgf, its builds and its tests at below-normal priority
    #[arg(long)]
    low_priority: bool,
    /// With --run, test survivors again even when their code and tests did not change
    #[arg(long)]
    rerun_survivors: bool,
    /// With --run, give every mutant a test run of its own instead of packing several into one
    #[arg(long)]
    no_mix: bool,
    /// With --changes: every mutant of a changed function (default), or only
    /// mutants on changed lines
    #[arg(long, value_enum)]
    scope: Option<mutate::Scope>,
    /// With --run, test every mutant again instead of reusing cached results
    #[arg(long)]
    no_cache: bool,
    /// `dotnet test` filter for the runs: "FILTER" for every test project, or
    /// "Project.Tests:FILTER" for one (repeatable; replaces `test_filter` in dgf.toml)
    #[arg(long, value_name = "FILTER")]
    test_filter: Vec<String>,
    /// Leave out test projects matching this glob, such as "**/*.Acceptance.Tests.csproj" (repeatable)
    #[arg(long, value_name = "GLOB")]
    exclude_test_project: Vec<String>,
    /// Stop after this many minutes, keep what was tested, and exit with code 4
    #[arg(long, value_name = "N")]
    max_minutes: Option<f64>,
    /// What the sandbox copies [default: `sandbox` in dgf.toml, else closure]
    #[arg(long, value_enum)]
    sandbox: Option<config::SandboxMode>,
    /// Which mutators run: basic, standard or advanced [default: `level` in
    /// dgf.toml, else standard]
    #[arg(long, value_name = "LEVEL")]
    level: Option<String>,
    #[command(flatten)]
    out: OutputArgs,
    #[command(flatten)]
    limits: LimitArgs,
}

#[derive(clap::Args)]
struct BaselineArgs {
    /// Files or directories to analyze (.gitignore is respected)
    #[arg(default_value = ".")]
    paths: Vec<PathBuf>,
    /// Where to write the baseline [default: .dgf/baseline.json next to dgf.toml]
    #[arg(long, value_name = "FILE")]
    file: Option<PathBuf>,
    #[command(flatten)]
    limits: LimitArgs,
}

const DEFAULT_TOP: usize = 20;
const EXIT_FAIL: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_IO: u8 = 3;
const EXIT_BUDGET: u8 = 4;

// dgf-ignore: one match arm per subcommand; splitting it would only hide the list
fn main() -> ExitCode {
    let cli = Cli::parse();
    work_in_real_folder();
    let result = match cli.command {
        Command::Analyze(args) => analyze(args),
        Command::Changes(args) => compare(args),
        Command::History(args) => overview(args),
        Command::Baseline(args) => accept(args),
        Command::Mutate(args) => mutation(args),
        Command::Hook { action } => hook(action),
        Command::Skill { action } => skill(action),
        Command::Init => init(),
    };
    result.unwrap_or_else(|(code, message)| {
        eprintln!("error: {message}");
        ExitCode::from(code)
    })
}

type Failure = (u8, String);

fn usage(message: String) -> Failure {
    (EXIT_USAGE, message)
}

fn io(message: String) -> Failure {
    (EXIT_IO, message)
}

fn exit_code(failed: bool) -> ExitCode {
    if failed {
        ExitCode::from(EXIT_FAIL)
    } else {
        ExitCode::SUCCESS
    }
}

fn init() -> Result<ExitCode, Failure> {
    let path = Path::new(config::FILE_NAME);
    if path.exists() {
        return Err(usage(format!("{} already exists", config::FILE_NAME)));
    }
    std::fs::write(path, config::TEMPLATE)
        .map_err(|err| io(format!("could not write {}: {err}", path.display())))?;
    println!("Created {}", config::FILE_NAME);
    Ok(ExitCode::SUCCESS)
}

fn analyze(args: AnalyzeArgs) -> Result<ExitCode, Failure> {
    only_for_mutate(args.out.format)?;
    let settings = load_settings(&args.limits)?;
    let top = args
        .top
        .or(settings.config.analyze.top)
        .unwrap_or(DEFAULT_TOP);
    let coverage = load_coverage(&args.coverage, &settings)?;
    let baseline = if args.no_baseline {
        None
    } else {
        Baseline::load(&settings.config_dir.join(baseline::DEFAULT_FILE)).map_err(usage)?
    };

    let started = Instant::now();
    let mut settings = settings;
    let (reports, warnings) = scan(&args.paths, std::mem::take(&mut settings.excludes))?;
    let mut rows = rows(&reports, &settings.limits);
    if let Some(baseline) = &baseline {
        baseline::apply(&mut rows, baseline, &settings.config_dir);
    }
    if let Some(coverage) = &coverage {
        apply_coverage(&mut rows, coverage, &settings.limits);
    }
    apply_mutation(
        &mut rows,
        &git::root(&settings.cwd).unwrap_or_else(|_| settings.cwd.clone()),
        &settings,
    );
    let run = Run {
        paths: args
            .paths
            .iter()
            .map(|p| display_path(&p.to_string_lossy()))
            .collect(),
        coverage: coverage
            .map(|c| {
                c.sources
                    .iter()
                    .map(|s| display_path(&s.to_string_lossy()))
                    .collect()
            })
            .unwrap_or_default(),
        rows,
        files: reports.len(),
        elapsed: started.elapsed(),
        limits: &settings.limits,
        config: settings
            .config_path
            .as_ref()
            .map(|path| display_path(&path.to_string_lossy())),
        warnings,
    };

    write_report(&report::render(&run, args.out.format, top), &args.out)?;
    print_warnings(&run.warnings, args.out.format);
    if args.save_history {
        save_history(&run, &settings.config_dir, &settings.history_dir)?;
    }
    Ok(exit_code(run.count(Status::Fail) > 0))
}

/// Finds and analyzes the files under `paths`.
fn scan(paths: &[PathBuf], excludes: Excludes) -> Result<(Vec<FileReport>, Vec<String>), Failure> {
    if let Some(missing) = paths.iter().find(|p| !p.exists()) {
        return Err(io(format!("path not found: {}", missing.display())));
    }
    let files = discover::discover(paths, excludes);
    Ok(analyze_files(&files))
}

/// `--coverage` paths, else `[coverage] paths` from dgf.toml; none means no coverage.
fn load_coverage(args: &[PathBuf], settings: &Settings) -> Result<Option<Coverage>, Failure> {
    let paths: Vec<PathBuf> = if args.is_empty() {
        settings
            .config
            .coverage
            .paths
            .iter()
            .map(|p| settings.config_dir.join(p))
            .collect()
    } else {
        args.to_vec()
    };
    if paths.is_empty() {
        return Ok(None);
    }
    Coverage::load(&paths).map(Some).map_err(usage)
}

/// Adds coverage and a CRAP score to every row the reports cover; a CRAP score
/// above its limit raises the status (ignored functions stay ignored).
fn apply_coverage(rows: &mut [Row], coverage: &Coverage, limits: &Limits) {
    for row in rows {
        let Some(lines) = coverage.for_file(Path::new(&row.file)) else {
            continue;
        };
        let f = row.function;
        row.coverage = coverage::ratio(lines, f.start_line, f.end_line);
        row.crap = row
            .coverage
            .map(|ratio| coverage::crap(f.complexity, ratio));
        if let (Some(crap), true) = (row.crap, row.status != Status::Ignored) {
            row.status = row.status.max(limits.crap.status(crap));
        }
    }
}

/// Mutation scores from the mutation cache, for functions whose code has not
/// changed since `dgf mutate --run` tested them.
fn apply_mutation(rows: &mut [Row], root: &Path, settings: &Settings) {
    let dir = cache_dir(root, settings);
    if !dir.exists() {
        return;
    }
    let cache = mutation::cache::Cache::load(&dir);
    let mut sources: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    for row in rows {
        let source = sources
            .entry(row.file.clone())
            .or_insert_with(|| std::fs::read_to_string(&row.file).ok());
        let Some(source) = source else {
            continue;
        };
        let lines = (row.function.start_line, row.function.end_line);
        let Some((_, text)) = mutation::cache::function_text(source, lines.0, lines.1) else {
            continue;
        };
        let relative = baseline::relative(root, &row.file);
        row.mutation = cache
            .score(&relative, &row.function.name, text)
            .map(|(score, _)| score);
    }
}

/// `dgf changes`
fn compare(args: ChangesArgs) -> Result<ExitCode, Failure> {
    only_for_mutate(args.out.format)?;
    let settings = load_settings(&args.limits)?;
    let root = git::root(&settings.cwd).map_err(usage)?;
    let run = changes::collect(changes::Request {
        base: base_for(&args, &settings.config, &root)?,
        root: &root,
        staged: args.staged,
        scope: args
            .scope
            .or(settings.config.changes.scope)
            .unwrap_or(Scope::Lines),
        fail_on: args
            .fail_on
            .or(settings.config.changes.fail_on)
            .unwrap_or(FailOn::NewOrWorse),
        excludes: &settings.excludes,
        limits: &settings.limits,
    })
    .map_err(usage)?;

    write_report(&report::changes::render(&run, args.out.format), &args.out)?;
    print_warnings(&run.warnings, args.out.format);
    Ok(exit_code(run.verdict() == Status::Fail))
}

/// `--base`, else HEAD for staged changes, else `[changes] base`, else main or master.
fn base_for(args: &ChangesArgs, config: &Config, root: &Path) -> Result<String, Failure> {
    let staged_base = args.staged.then(|| "HEAD".to_string());
    args.base
        .clone()
        .or(staged_base)
        .or_else(|| config.changes.base.clone())
        .or_else(|| git::default_base(root))
        .ok_or_else(|| {
            usage("no `main` or `master` branch to compare with; pass --base".to_string())
        })
}

/// `dgf history`
fn overview(args: HistoryArgs) -> Result<ExitCode, Failure> {
    let (cwd, config_path, config) = load_config(args.no_config)?;
    let config_dir = config_dir(&cwd, config_path.as_deref());
    let dir = args
        .dir
        .clone()
        .unwrap_or_else(|| history_dir(&config_dir, &config));
    let (snapshots, warnings) = history::load(&dir).map_err(io)?;
    let overview = history::Overview::build(snapshots, warnings);
    let top = args.top.or(config.analyze.top).unwrap_or(DEFAULT_TOP);
    let text = report::history::render(&overview, args.out.format, top).map_err(usage)?;
    write_report(&text, &args.out)?;
    print_warnings(&overview.warnings, args.out.format);
    Ok(ExitCode::SUCCESS)
}

/// `dgf baseline`
fn accept(args: BaselineArgs) -> Result<ExitCode, Failure> {
    let settings = load_settings(&args.limits)?;
    let (reports, warnings) = scan(&args.paths, settings.excludes)?;
    let rows = rows(&reports, &settings.limits);
    let baseline = Baseline::from_rows(&rows, &settings.config_dir, report::now());
    let path = args
        .file
        .clone()
        .unwrap_or_else(|| settings.config_dir.join(baseline::DEFAULT_FILE));
    baseline.save(&path).map_err(io)?;
    print_warnings(&warnings, Format::Table);
    println!(
        "Accepted {} functions above the limits in {}. Later runs ignore them until their score goes up.",
        baseline.functions.len(),
        display_path(&path.to_string_lossy())
    );
    Ok(ExitCode::SUCCESS)
}

/// `--format mutation-report` describes mutants: other commands refuse it.
fn only_for_mutate(format: Format) -> Result<(), Failure> {
    if format == Format::MutationReport {
        return Err(usage(
            "--format mutation-report is only for dgf mutate --run".to_string(),
        ));
    }
    Ok(())
}

/// `dgf mutate`
fn mutation(args: MutateArgs) -> Result<ExitCode, Failure> {
    let mut settings = load_settings(&args.limits)?;
    let mut never = Excludes::default();
    never
        .add(&settings.config_dir, &settings.config.mutate.exclude)
        .map_err(usage)?;
    let wanted = if args.changes {
        changed_functions(&args, &settings)?
    } else {
        let (reports, _) = scan(&args.paths, std::mem::take(&mut settings.excludes))?;
        mutate::from_reports(&reports, args.min_complexity, display_path)
    };
    let found = wanted.len();
    let wanted: Vec<mutate::Wanted> = wanted
        .into_iter()
        .filter(|w| !never.is_excluded(&w.path, false))
        .collect();
    if !args.limits.no_config {
        eprintln!("{}", config_used(&settings, found - wanted.len()));
    }
    let (targets, warnings) =
        mutate::targets(wanted, &mutator_settings(&args, &settings)?).map_err(usage)?;
    let root = git::root(&settings.cwd).unwrap_or_else(|_| settings.cwd.clone());
    mutate::check_named_tests(&targets, &root).map_err(usage)?;
    print_warnings(&warnings, args.out.format);
    if args.run {
        return run_mutants(targets, &args, &settings);
    }
    let text = mutate::render(&targets, args.out.format, args.show).map_err(usage)?;
    write_report(&text, &args.out)?;
    Ok(ExitCode::SUCCESS)
}

/// Which dgf.toml a mutation run follows and what its excludes left out, so
/// a run that found none (a worktree of a branch older than the file, a
/// command run from another folder) does not pass for one that did.
fn config_used(settings: &Settings, left_out: usize) -> String {
    match &settings.config_path {
        Some(path) => format!(
            "note: following {}; [mutate] exclude left out {left_out} functions",
            path.display()
        ),
        None => format!(
            "warning: no dgf.toml in {} or above it, so nothing is excluded and the defaults apply; run dgf inside the checkout that has one",
            settings.cwd.display()
        ),
    }
}

/// Which mutants to make: `--level`, then `[mutate]` in dgf.toml.
fn mutator_settings(
    args: &MutateArgs,
    settings: &Settings,
) -> Result<dog_food_core::mutate::Settings, Failure> {
    let config = &settings.config.mutate;
    let level = match args.level.as_deref().or(config.level.as_deref()) {
        Some(text) => text.parse().map_err(usage)?,
        None => dog_food_core::mutate::Level::default(),
    };
    if let Some(unknown) = config
        .ignore_mutators
        .iter()
        .find(|n| !dog_food_core::mutate::is_known(n))
    {
        return Err(usage(format!(
            "[mutate] ignore_mutators: unknown mutator {unknown:?}"
        )));
    }
    Ok(dog_food_core::mutate::Settings {
        level,
        ignore_mutators: config.ignore_mutators.clone(),
        ignore_methods: config.ignore_methods.clone(),
    })
}

/// Settings of `dgf mutate --run`: flags first, then `[mutate]` in dgf.toml.
fn mutation_options(args: &MutateArgs, settings: &Settings) -> mutation::Options {
    let root = git::root(&settings.cwd).unwrap_or_else(|_| settings.cwd.clone());
    let config = &settings.config.mutate;
    let filters = if args.test_filter.is_empty() {
        config.test_filter.clone()
    } else {
        args.test_filter.clone()
    };
    let mut exclude = config.exclude_test_projects.clone();
    exclude.extend(args.exclude_test_project.iter().cloned());
    let defaults = mutation::Timeouts::default();
    mutation::Options {
        cache_dir: cache_dir(&root, settings),
        jobs: args.jobs.or(config.jobs).unwrap_or_else(default_jobs),
        build_jobs: args.build_jobs.or(config.build_jobs),
        timeouts: mutation::Timeouts {
            factor: args
                .timeout_factor
                .or(config.timeout_factor)
                .unwrap_or(defaults.factor),
            extra: args
                .timeout_ms
                .or(config.timeout_ms)
                .map_or(defaults.extra, std::time::Duration::from_millis),
        },
        rerun_survivors: args.rerun_survivors,
        no_mix: args.no_mix,
        low_priority: args.low_priority || config.low_priority,
        use_cache: !args.no_cache,
        tests: mutation::TestSelection {
            filters,
            include: config.test_projects.clone(),
            exclude,
        },
        sandbox: args.sandbox.or(config.sandbox).unwrap_or_default(),
        budget: mutation::Budget::minutes(args.max_minutes.or(config.max_minutes)),
        root,
    }
}

/// One core is left for the rest of the machine.
fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get().saturating_sub(1).max(1))
}

/// `[mutate] cache_dir` relative to dgf.toml (`~/` is the home folder), else
/// `.dgf/mutation-cache` in the repository.
fn cache_dir(root: &Path, settings: &Settings) -> PathBuf {
    let Some(dir) = settings.config.mutate.cache_dir.as_deref() else {
        return root.join(mutation::cache::DEFAULT_DIR);
    };
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    match (dir.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => settings.config_dir.join(dir),
    }
}

/// `dgf mutate --run`: C# (dotnet test) and TypeScript/JavaScript (Vitest, Jest).
fn run_mutants(
    mut targets: Vec<mutate::Target>,
    args: &MutateArgs,
    settings: &Settings,
) -> Result<ExitCode, Failure> {
    let others = targets
        .iter()
        .filter(|t| !mutation::runnable(&t.path))
        .count();
    if others > 0 {
        eprintln!("note: {others} Rust functions are skipped; use cargo-mutants for Rust");
    }
    targets.retain(|t| mutation::runnable(&t.path) && !t.mutants.is_empty());
    if targets.is_empty() {
        return Err(usage(
            "no C# or TypeScript functions with mutants to run".to_string(),
        ));
    }
    let options = mutation_options(args, settings);
    if options.low_priority {
        mutation::priority::lower().map_err(usage)?;
    }
    let count: usize = targets.iter().map(|t| t.mutants.len()).sum();
    eprintln!("Testing {count} mutants in {} functions...", targets.len());
    let result = mutation::run(&mut targets, &options).map_err(usage)?;
    let mut thresholds = settings.config.mutate.thresholds;
    thresholds.fail = args.fail_under.or(thresholds.fail);
    let text =
        mutate::render_run(&targets, &result, args.out.format, &thresholds).map_err(usage)?;
    write_report(&text, &args.out)?;
    if result.budget_hit() {
        return Ok(ExitCode::from(EXIT_BUDGET));
    }
    // Below `break` fails; no score at all never does.
    let score = mutate::overall(&targets, &result);
    Ok(exit_code(
        thresholds
            .fail
            .is_some_and(|limit| score.is_some_and(|s| s < limit)),
    ))
}

/// A branch that only changes tests changes what the mutants of the code
/// they test would show: those functions come back. Found through the cache,
/// which records the test files that reached each mutant.
fn add_tested_by_changed_tests(
    wanted: &mut Vec<mutate::Wanted>,
    root: &Path,
    base_commit: &str,
    settings: &Settings,
) -> Result<(), Failure> {
    let changed: Vec<String> = git::changed_files(root, base_commit, false)
        .map_err(usage)?
        .into_iter()
        .map(|c| c.path)
        .filter(|p| mutate::is_test_code(Path::new(p)))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    let cache = mutation::cache::Cache::load(&cache_dir(root, settings));
    for (file, name) in cache.reached_by(&changed) {
        let known = wanted
            .iter()
            .any(|w| w.file == file && changes::stable_name(&w.name) == name);
        if !known {
            wanted.extend(mutate::function_named(root, &file, &name));
        }
    }
    Ok(())
}

/// The functions `dgf changes` reports for this branch.
fn changed_functions(
    args: &MutateArgs,
    settings: &Settings,
) -> Result<Vec<mutate::Wanted>, Failure> {
    let root = git::root(&settings.cwd).map_err(usage)?;
    let base = args
        .base
        .clone()
        .or_else(|| settings.config.changes.base.clone())
        .or_else(|| git::default_base(&root))
        .ok_or_else(|| {
            usage("no `main` or `master` branch to compare with; pass --base".to_string())
        })?;
    let run = changes::collect(changes::Request {
        root: &root,
        base,
        staged: false,
        scope: Scope::Lines,
        fail_on: FailOn::NewOrWorse,
        excludes: &settings.excludes,
        limits: &settings.limits,
    })
    .map_err(usage)?;
    for warning in &run.warnings {
        eprintln!("warning: {warning}");
    }
    let mut wanted = mutate::from_changes(&run, &root, args.min_complexity);
    add_tested_by_changed_tests(&mut wanted, &root, &run.base_commit, settings)?;
    if args.scope == Some(mutate::Scope::Lines) {
        let changed = git::changed_files(&root, &run.base_commit, false).map_err(usage)?;
        for w in &mut wanted {
            w.lines = changed
                .iter()
                .find(|c| c.path == w.file)
                .map(|c| c.ranges.clone());
        }
    }
    Ok(wanted)
}

/// Moves to the real path of the current folder when it was reached through
/// a link (a junction or symlink to a worktree): git names files by their
/// real path, so dgf.toml's excludes and limits must be anchored there too.
fn work_in_real_folder() {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    if let Some(real) = discover::real(&cwd).filter(|r| *r != cwd) {
        let _ = std::env::set_current_dir(real);
    }
}

/// `dgf hook install|uninstall`
fn hook(action: HookAction) -> Result<ExitCode, Failure> {
    let cwd = std::env::current_dir().map_err(|err| io(err.to_string()))?;
    let root = git::root(&cwd).map_err(usage)?;
    match action {
        HookAction::Install { force } => {
            let path = setup::install_hook(&root, force).map_err(usage)?;
            println!("Installed {}: commits that add or worsen functions above the fail limit are blocked.", path.display());
        }
        HookAction::Uninstall => {
            let path = setup::uninstall_hook(&root).map_err(usage)?;
            println!("Removed {}", path.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `dgf skill install|show`
fn skill(action: SkillAction) -> Result<ExitCode, Failure> {
    let SkillAction::Install { global } = action else {
        print!("{}", setup::SKILL);
        return Ok(ExitCode::SUCCESS);
    };
    let cwd = std::env::current_dir().map_err(|err| io(err.to_string()))?;
    let root = git::root(&cwd).unwrap_or(cwd);
    let path = setup::install_skill(&root, global).map_err(io)?;
    println!(
        "Installed the dog-food skill at {}",
        display_path(&path.to_string_lossy())
    );
    Ok(ExitCode::SUCCESS)
}

fn save_history(run: &Run, config_dir: &Path, dir: &Path) -> Result<(), Failure> {
    let (commit, branch) = git::root(config_dir)
        .map(|root| git::head(&root))
        .unwrap_or((None, None));
    let snapshot = history::Snapshot::from_run(run, report::now(), commit, branch);
    let path = history::save(dir, &snapshot).map_err(io)?;
    eprintln!(
        "Saved history snapshot {}",
        display_path(&path.to_string_lossy())
    );
    Ok(())
}

/// Prints the report, or writes it to `--output`.
fn write_report(text: &str, out: &OutputArgs) -> Result<(), Failure> {
    let Some(path) = &out.output else {
        print!("{text}");
        return Ok(());
    };
    std::fs::write(path, text)
        .map_err(|err| io(format!("could not write {}: {err}", path.display())))?;
    eprintln!("Wrote {} report to {}", out.format.name(), path.display());
    Ok(())
}

/// JSON, SARIF and agent output carry their warnings inside or have no place
/// for them; the human formats print them to stderr.
fn print_warnings(warnings: &[String], format: Format) {
    if matches!(format, Format::Table | Format::Markdown | Format::Html) {
        for warning in warnings {
            eprintln!("warning: {warning}");
        }
    }
}

/// dgf.toml (unless --no-config) combined with the command-line flags.
struct Settings {
    cwd: PathBuf,
    config_path: Option<PathBuf>,
    config_dir: PathBuf,
    config: Config,
    excludes: Excludes,
    limits: Limits,
    history_dir: PathBuf,
}

fn load_config(no_config: bool) -> Result<(PathBuf, Option<PathBuf>, Config), Failure> {
    let cwd = std::env::current_dir().map_err(|err| io(err.to_string()))?;
    let config_path = (!no_config).then(|| config::find(&cwd)).flatten();
    let config = match &config_path {
        Some(path) => config::load(path).map_err(usage)?,
        None => Config::default(),
    };
    Ok((cwd, config_path, config))
}

/// Paths in dgf.toml are relative to the file; without one, to the current folder.
fn config_dir(cwd: &Path, config_path: Option<&Path>) -> PathBuf {
    config_path
        .and_then(Path::parent)
        .unwrap_or(cwd)
        .to_path_buf()
}

fn history_dir(config_dir: &Path, config: &Config) -> PathBuf {
    config_dir.join(
        config
            .history
            .dir
            .as_deref()
            .unwrap_or(history::DEFAULT_DIR),
    )
}

fn load_settings(args: &LimitArgs) -> Result<Settings, Failure> {
    let (cwd, config_path, config) = load_config(args.no_config)?;
    let config_dir = config_dir(&cwd, config_path.as_deref());

    let mut excludes = Excludes::default();
    excludes.add(&cwd, DEFAULT_EXCLUDES).map_err(usage)?;
    excludes
        .add(&config_dir, &config.analyze.exclude)
        .map_err(usage)?;
    excludes.add(&cwd, &args.excludes).map_err(usage)?;

    let limits = Limits::new(&config.limits, &config_dir, args.warn, args.fail).map_err(usage)?;
    let history_dir = history_dir(&config_dir, &config);
    Ok(Settings {
        cwd,
        config_path,
        config_dir,
        config,
        excludes,
        limits,
        history_dir,
    })
}

/// Analyzes files in parallel; files that fail become warnings.
fn analyze_files(files: &[PathBuf]) -> (Vec<FileReport>, Vec<String>) {
    let results: Vec<_> = files
        .par_iter()
        .map(|path| dog_food_core::analyze_file(path))
        .collect();
    let mut reports = Vec::new();
    let mut warnings = Vec::new();
    for result in results {
        match result {
            Ok(report) => {
                if report.has_errors {
                    warnings.push(format!(
                        "{}: syntax errors, results may be partial",
                        display_path(&report.path)
                    ));
                }
                reports.push(report);
            }
            Err(err) => warnings.push(err.to_string()),
        }
    }
    (reports, warnings)
}

/// One row per function, most complex first.
fn rows<'a>(reports: &'a [FileReport], limits: &Limits) -> Vec<Row<'a>> {
    let mut rows: Vec<Row> = reports
        .iter()
        .flat_map(|report| {
            let level = limits.for_file(Path::new(&report.path));
            let file = display_path(&report.path);
            report
                .functions
                .iter()
                .map(move |function| Row::new(file.clone(), report, function, level))
        })
        .collect();
    rows.sort_by(|a, b| {
        b.function
            .complexity
            .cmp(&a.function.complexity)
            .then_with(|| a.file.cmp(&b.file))
    });
    rows
}

/// Forward slashes and no leading `./`, so reports look the same on every OS.
fn display_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.strip_prefix("./").map(str::to_string).unwrap_or(path)
}
