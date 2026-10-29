//! `dgf mutate --run`: the mutation engine.
//!
//! 1. Work out what each language's run needs (`scope`): the test projects
//!    and their filters, the folders to copy, the fingerprints for the cache.
//! 2. Copy those folders to a sandbox and rewrite the chosen functions so
//!    every mutant has a switch (`sandbox`).
//! 3. Build once (C#; `build` takes broken mutants out) and run the tests once
//!    with no mutant active, recording which test reaches which mutant (`Reach`).
//! 4. Pack mutants into test runs: inside one run every test carries at most
//!    one mutant, so mutants reached by different tests share a run (`pack`).
//! 5. Run the packs in parallel; a mutant whose tests did not all report, or
//!    that code outside any test reaches, gets a run of its own (`solo`).
//!
//! Results are cached per mutant (`cache`). A time budget stops the run
//! cleanly: what was tested is kept, the rest is reported as untested.

mod build;
pub mod cache;
mod dotnet;
mod exec;
mod node;
pub mod priority;
pub mod projects;
mod sandbox;
mod scope;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::Serialize;

use crate::config::SandboxMode;
use crate::mutate::Target;
pub use projects::TestSelection;
use sandbox::FileMutants;
use scope::Scope;

/// What happened to one mutant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// A test failed: the tests notice this change.
    Killed,
    /// Every test passed: a gap in the tests.
    Survived,
    /// The tests ran too long (often an endless loop); counts as killed.
    Timeout,
    /// No test runs this code at all.
    NoCoverage,
    /// The mutant does not compile; not counted.
    CompileError,
    /// The test run crashed before its tests reported; not counted.
    RuntimeError,
    /// Turned off by a `dgf-disable` comment, `ignore_methods` or
    /// `[ExcludeFromCodeCoverage]`: not tested, not counted.
    Ignored,
}

impl Outcome {
    /// Whether the outcome counts in the mutation score.
    pub fn counts(self) -> bool {
        !matches!(
            self,
            Outcome::CompileError | Outcome::RuntimeError | Outcome::Ignored
        )
    }

    /// A survivor: no test noticed the mutant.
    pub fn undetected(self) -> bool {
        matches!(self, Outcome::Survived | Outcome::NoCoverage)
    }

    /// What `--rerun-survivors` tests again: survivors and crashed runs.
    pub fn retested_on_rerun(self) -> bool {
        self.undetected() || self == Outcome::RuntimeError
    }

    /// Whether the outcome is worth caching: an ignored mutant was not
    /// tested. A crash is kept too, so a render in another format does not
    /// test it again; `--rerun-survivors` does.
    fn is_cached(self) -> bool {
        self != Outcome::Ignored
    }
}

pub struct Options {
    pub root: PathBuf,
    pub jobs: usize,
    pub use_cache: bool,
    pub cache_dir: PathBuf,
    pub tests: TestSelection,
    pub sandbox: SandboxMode,
    pub budget: Budget,
    pub timeouts: Timeouts,
    /// `dotnet build -m:N`.
    pub build_jobs: Option<usize>,
    /// Builds and tests at below-normal priority.
    pub low_priority: bool,
    /// Test cached survivors again.
    pub rerun_survivors: bool,
    /// One mutant per test run.
    pub no_mix: bool,
    /// Report the cached results only; test nothing (`--cached`).
    pub cached_only: bool,
}

/// When a test run counts as hanging: (startup +
/// the time its tests took without mutants) × `factor` + `extra`.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub factor: f64,
    pub extra: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            factor: 1.5,
            extra: Duration::from_millis(5000),
        }
    }
}

impl Timeouts {
    /// The limit for a run of `tests` (every test when `None`).
    fn limit(&self, reach: &Reach, tests: Option<&BTreeSet<String>>) -> Duration {
        let chosen: Vec<&String> = match tests {
            Some(tests) => tests.iter().collect(),
            None => reach.times.keys().collect(),
        };
        let homes: BTreeSet<&String> = chosen.iter().filter_map(|t| reach.home.get(*t)).collect();
        let startup: Duration = homes.iter().filter_map(|h| reach.startup.get(*h)).sum();
        let time: Duration = chosen.iter().filter_map(|t| reach.times.get(*t)).sum();
        (startup + time).mul_f64(self.factor) + self.extra
    }
}

/// When the run has to stop (`--max-minutes`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Budget {
    deadline: Option<Instant>,
}

impl Budget {
    pub fn minutes(minutes: Option<f64>) -> Budget {
        Budget {
            deadline: minutes.map(|m| Instant::now() + Duration::from_secs_f64(m.max(0.0) * 60.0)),
        }
    }

    pub fn spent(&self) -> bool {
        self.deadline.is_some_and(|d| Instant::now() >= d)
    }

    /// `wanted`, or less when the budget ends sooner.
    pub fn limit(&self, wanted: Duration) -> Duration {
        self.deadline.map_or(wanted, |d| {
            wanted.min(d.saturating_duration_since(Instant::now()))
        })
    }
}

/// The step a run was in when the budget ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Untested {
    /// Building the sandbox.
    Build,
    /// The test run that records which test reaches which mutant.
    Baseline,
    /// The mutant's own test runs had not started or finished.
    TestRuns,
    /// No Vitest or Jest package above the file: nothing can test it.
    NoRunner,
    /// `--cached`: no cached result, and nothing was tested.
    NotCached,
}

#[derive(Default)]
pub struct RunResult {
    pub outcomes: HashMap<usize, Outcome>,
    /// Mutants whose outcome came from the cache.
    pub cached: HashSet<usize>,
    /// Mutants the budget left untested, and the step that did not finish.
    pub untested: BTreeMap<usize, Untested>,
    pub sandboxes: Vec<PathBuf>,
    pub test_projects: Vec<String>,
    pub prepare: Duration,
    pub build: Duration,
    pub baseline: Duration,
    pub mutants: Duration,
    /// Test runs started for mutants (packs plus solo runs).
    pub test_runs: usize,
    /// Of those, second runs of a solo run that timed out (a busy machine
    /// or an endless loop: the second, longer run tells them apart).
    pub retries: usize,
    /// Tests that fail without mutants, left out.
    pub set_aside: Vec<String>,
    /// Killed in a run whose tests did not all report (the test host crashed
    /// or was stopped): a failing test decided it, but the crash may have.
    pub crash_kills: BTreeSet<usize>,
    /// Things the reader should know, such as a fallback to a full sandbox.
    pub notes: Vec<String>,
    /// Per tested mutant, up to three tests that reach it ("name (file)").
    pub reached: HashMap<usize, Vec<String>>,
    /// Per tested mutant, the files of every test that reaches it.
    pub reached_files: HashMap<usize, BTreeSet<String>>,
    pub jobs: usize,
    /// When every result came from the cache: when the run whose timings
    /// are reported ended (seconds since 1970).
    pub timings_from: Option<u64>,
}

impl RunResult {
    /// Whether `--max-minutes` stopped the run.
    pub fn budget_hit(&self) -> bool {
        self.untested
            .values()
            .any(|step| !matches!(step, Untested::NoRunner | Untested::NotCached))
    }
}

/// Which tests reach which mutant, from the run with no mutant active.
#[derive(Debug, Default, Clone)]
pub struct Reach {
    pub by_mutant: HashMap<usize, BTreeSet<String>>,
    /// Mutants reached outside any test (static setup, module load): they run
    /// with every test.
    pub outside: HashSet<usize>,
    /// Where each test lives: its test project (C#) or test file (TypeScript).
    pub home: HashMap<String, String>,
    /// Each test's source files, relative to the repository, when known.
    pub files: HashMap<String, Vec<String>>,
    /// Tests that fail with no mutant active: left out of every decision.
    pub failing: BTreeSet<String>,
    pub duration: Duration,
    /// Each test's time in the run without mutants.
    pub times: HashMap<String, Duration>,
    /// Per test home (project or file): what a run of it takes besides its
    /// tests (starting the test host, discovery).
    pub startup: HashMap<String, Duration>,
}

impl Reach {
    /// Leaves out the tests that fail anyway: they cannot tell a mutant apart.
    fn set_aside_failing(&mut self) {
        let failing = &self.failing;
        for tests in self.by_mutant.values_mut() {
            tests.retain(|t| !failing.contains(t));
        }
        self.by_mutant.retain(|_, tests| !tests.is_empty());
    }
}

fn failing_tests(outcome: &HashMap<String, bool>) -> BTreeSet<String> {
    outcome
        .iter()
        .filter(|(_, passed)| !**passed)
        .map(|(test, _)| test.clone())
        .collect()
}

/// The baseline run must have run tests: when nothing reported, or it ran
/// for an hour, there is nothing to compare mutants with. When half the
/// tests or more fail without any mutant, the results would say little, so
/// the run stops.
fn check_baseline(status: TestRun, outcome: &HashMap<String, bool>) -> Result<(), String> {
    if status == TestRun::TimedOut || outcome.is_empty() {
        return Err(
            "the tests did not run without mutants; run them once yourself to see why".into(),
        );
    }
    let failing = outcome.values().filter(|passed| !**passed).count();
    if failing * 2 >= outcome.len() {
        return Err(format!(
            "{failing} of {} tests fail without any mutant; fix them or leave them out with --test-filter",
            outcome.len()
        ));
    }
    Ok(())
}

/// Mutants that share one test run: each test carries at most one mutant.
#[derive(Debug, Default)]
pub struct Pack {
    pub plan: Vec<(String, usize)>,
    pub mutants: Vec<usize>,
    used: HashSet<String>,
}

/// Result of one test run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestRun {
    Passed,
    Failed,
    TimedOut,
}

/// Per-test results of a pack: `None` when the whole run timed out.
pub type PackRun = Option<HashMap<String, bool>>;

/// A solo run: how the run ended and every test's result (test → passed).
pub type SoloRun = (TestRun, HashMap<String, bool>);

/// A mutant's verdict from a solo run, and the tests that killed it.
/// A test that passes without mutants failed: killed. Tests that fail anyway
/// do not count. A run that ended without a report from every test it was
/// meant to run crashed: a runtime error.
fn solo_outcome(
    (status, results): &SoloRun,
    failing: &BTreeSet<String>,
    expected: Option<&BTreeSet<String>>,
) -> (Outcome, Vec<String>) {
    if *status == TestRun::TimedOut {
        return (Outcome::Timeout, Vec::new());
    }
    let killers: Vec<String> = results
        .iter()
        .filter(|(test, passed)| !**passed && !failing.contains(*test))
        .map(|(test, _)| test.clone())
        .collect();
    if !killers.is_empty() {
        return (Outcome::Killed, killers);
    }
    if crashed(*status, results, expected) {
        return (Outcome::RuntimeError, Vec::new());
    }
    (Outcome::Survived, Vec::new())
}

/// No test reported, or the run failed with tests it should have run missing.
fn crashed(
    status: TestRun,
    results: &HashMap<String, bool>,
    expected: Option<&BTreeSet<String>>,
) -> bool {
    let missing = expected.is_some_and(|tests| tests.iter().any(|t| !results.contains_key(t)));
    results.is_empty() || status == TestRun::Failed && missing
}

/// Why a build did not succeed.
#[derive(Debug)]
pub enum BuildError {
    /// The compiler's error lines.
    Errors(Vec<String>),
    /// The time budget ran out while building.
    OutOfTime,
}

/// Which projects a build compiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Round {
    /// Everything the tests need.
    All,
    /// Only the projects with mutants (and whatever is out of date below them).
    Mutated,
}

/// What a language needs to provide.
pub trait Engine: Sync {
    /// Builds the sandbox.
    fn build(&self, round: Round, budget: Budget) -> Result<(), BuildError>;
    /// What compiler errors say about the mutants.
    fn locate(&self, files: &[FileMutants], errors: &[String]) -> exec::Located;
    /// Runs every test with no mutant active and records reach; `None` when
    /// the budget ran out first.
    fn reach(&self, budget: Budget) -> Result<Option<Reach>, String>;
    fn pack(
        &self,
        pack: &Pack,
        reach: &Reach,
        limit: Duration,
        tag: &str,
    ) -> Result<PackRun, String>;
    fn solo(
        &self,
        mutant: usize,
        reach: &Reach,
        limit: Duration,
        tag: &str,
    ) -> Result<SoloRun, String>;
    fn test_projects(&self) -> Vec<String>;
}

/// Most tests in one pack, so a run stays short enough to repeat.
const PACK_TESTS: usize = 200;

pub fn run(targets: &mut [Target], options: &Options) -> Result<RunResult, String> {
    number(targets);
    let mut result = RunResult {
        jobs: options.jobs,
        ..RunResult::default()
    };
    result.outcomes.extend(ignored(targets));
    let mut cache = cache::Cache::load(&options.cache_dir);
    cache.rerun = options.rerun_survivors;
    for language in [sandbox::Kind::CSharp, sandbox::Kind::Node] {
        let mut pending: Vec<&Target> = targets
            .iter()
            .filter(|t| sandbox::Kind::of(&t.path) == Some(language))
            .collect();
        if language == sandbox::Kind::Node {
            pending = with_runner(pending, options, &mut result);
        }
        if pending.is_empty() {
            continue;
        }
        let scope = Scope::new(language, &pending, options)?;
        if options.use_cache {
            let mut found = cache.lookup(&pending, &scope);
            // A survivor already tested again against these tests is not
            // tested a third time: renders of other formats stay cheap.
            let rechecked = if options.rerun_survivors {
                cache.rechecked(&pending, &scope)
            } else {
                HashSet::new()
            };
            found.retain(|id, outcome| {
                !result.outcomes.contains_key(id)
                    && !(options.rerun_survivors
                        && outcome.retested_on_rerun()
                        && !rechecked.contains(id))
            });
            let kept = found.keys().filter(|id| rechecked.contains(id)).count();
            if kept > 0 {
                result.notes.push(format!("--rerun-survivors: {kept} survivors (marked `cached`) were already tested again, and neither the test files of the chosen test projects nor the test filter changed since, so they come from the cache; tests added in another project or outside the filter count only once the filter or test projects include them (or with --no-cache)"));
            }
            result.cached.extend(found.keys().copied());
            let mut tests = cache.reached_tests(&pending, &scope);
            tests.retain(|id, _| found.contains_key(id));
            result.reached.extend(tests);
            result.outcomes.extend(found);
        }
        let open: Vec<usize> = pending
            .iter()
            .flat_map(|t| &t.mutants)
            .map(|m| m.id)
            .filter(|id| !result.outcomes.contains_key(id))
            .collect();
        let mut killed_by = cache::KilledBy::new();
        if options.cached_only {
            if !open.is_empty() {
                result.notes.push(format!(
                    "--cached: {} mutants have no cached result and were not tested; run without --cached to test them",
                    open.len()
                ));
            }
            result
                .untested
                .extend(open.iter().map(|id| (*id, Untested::NotCached)));
        } else if !open.is_empty() {
            let total = pending.iter().map(|t| t.mutants.len()).sum::<usize>();
            progress(&format!(
                "{} of {total} mutants come from the cache; testing the other {} (new or changed code, changed tests, or never tested; --cached tests nothing)",
                total - open.len(),
                open.len()
            ));
            killed_by = run_language(&scope, &pending, options, &mut result)?;
        }
        // Only what this run tested: cached outcomes keep the evidence they have.
        let fresh: HashMap<usize, Outcome> = result
            .outcomes
            .iter()
            .filter(|(id, _)| !result.cached.contains(id))
            .map(|(id, outcome)| (*id, *outcome))
            .collect();
        cache.store(
            &pending,
            &scope,
            &fresh,
            &killed_by,
            &result.reached_files,
            &result.reached,
        );
        note_left_out(&scope, &pending, &mut result);
    }
    note_filtered(targets, options, &mut result);
    timings(targets, options, &mut cache, &mut result);
    cache.save()?;
    Ok(result)
}

/// Mutants no test reaches may be reached by a test project the run left out.
fn note_left_out(scope: &Scope, targets: &[&Target], result: &mut RunResult) {
    if scope.left_out.is_empty() {
        return;
    }
    let unreached = targets
        .iter()
        .flat_map(|t| &t.mutants)
        .filter(|m| result.outcomes.get(&m.id) == Some(&Outcome::NoCoverage))
        .count();
    if unreached > 0 {
        // A filter naming a project wins over exclude_test_projects, but then
        // only named projects run: name the ones that ran as well.
        let flags: Vec<String> = scope
            .tests
            .iter()
            .map(|t| t.name())
            .chain(scope.left_out.iter().cloned())
            .map(|name| format!("--test-filter \"{name}:FullyQualifiedName~.\""))
            .collect();
        result.notes.push(format!(
            "{unreached} mutants no test reaches, but these test projects use the mutated code and did not run (test_projects, exclude_test_projects or a filter for another project): {}; their tests may reach them. To run them with the others: {} (narrow each filter as needed)",
            scope.left_out.join(", "),
            flags.join(" ")
        ));
    }
}

/// A test project whose filter selected no test makes every mutant only it
/// reaches look untested: say so, with the filter.
fn note_empty_filters(scope: &Scope, reach: &Reach, result: &mut RunResult) {
    let empty: Vec<String> = scope
        .tests
        .iter()
        .filter(|t| !reach.home.values().any(|home| *home == t.assembly))
        .filter_map(|t| t.filter.as_ref().map(|f| format!("{} ({f})", t.name())))
        .collect();
    if !empty.is_empty()
        && result
            .notes
            .iter()
            .all(|n| !n.contains("has no `Project:` prefix"))
    {
        result.notes.push(format!(
            "these test projects ran no test under their filter: {}; check the filter (stray quotes, a class name that changed), since mutants only they reach look untested",
            empty.join(", ")
        ));
    }
}

/// A baseline that ran no test says why it may have: test projects that
/// use the mutated code but were left out, and the filters of those that ran.
fn with_test_hint(err: String, scope: &Scope) -> String {
    let mut out = err;
    if !scope.left_out.is_empty() {
        out.push_str(&format!(
            "; these test projects use the mutated code but did not run: {} (a `--test-filter \"<Project>:<filter>\"` naming one runs it even when dgf.toml excludes it)",
            scope.left_out.join(", ")
        ));
    }
    let filtered: Vec<String> = scope
        .tests
        .iter()
        .filter_map(|t| t.filter.as_ref().map(|f| format!("{}: {f}", t.name())))
        .collect();
    if !filtered.is_empty() {
        out.push_str(&format!(
            "; the filters used: {} (a filter that matches no test, such as one with stray quotes, runs nothing)",
            filtered.join("; ")
        ));
    }
    out
}

/// A filter without `Project:` runs in every test project that uses the
/// mutated code; the ones where it matched no test only cost a test host
/// start each. Says which, and the flag when the filter is a project's name.
fn note_idle_projects(scope: &Scope, reach: &Reach, options: &Options, result: &mut RunResult) {
    let names: Vec<String> = scope.tests.iter().map(|t| t.name()).collect();
    let global: Vec<&String> = options
        .tests
        .filters
        .iter()
        .filter(|f| {
            f.split_once(':')
                .is_none_or(|(project, _)| !names.iter().any(|n| n == project.trim()))
        })
        .collect();
    if global.is_empty() {
        return;
    }
    let idle: Vec<String> = scope
        .tests
        .iter()
        .filter(|t| !reach.home.values().any(|home| *home == t.assembly))
        .map(|t| t.name())
        .collect();
    if idle.is_empty() {
        return;
    }
    let named = global
        .iter()
        .find(|f| names.iter().any(|n| n == f.trim()))
        .map(|f| {
            format!(
                "; `{f}` is a test project's name: to run only that project, pass `--test-filter \"{}:FullyQualifiedName~.\"`",
                f.trim()
            )
        })
        .unwrap_or_default();
    result.notes.push(format!(
        "the test filter ({}) has no `Project:` prefix, so it ran in every test project that uses the mutated code; these ran no test under it and only cost a start each: {}{named}",
        global.iter().map(|f| f.as_str()).collect::<Vec<_>>().join(", "),
        idle.join(", ")
    ));
}

/// Mutants no test reached under a test filter may only be outside the
/// filter: a filter on a class or a file misses the sibling classes.
fn note_filtered(targets: &[Target], options: &Options, result: &mut RunResult) {
    if options.tests.filters.is_empty() {
        return;
    }
    let unreached = targets
        .iter()
        .flat_map(|t| &t.mutants)
        .filter(|m| result.outcomes.get(&m.id) == Some(&Outcome::NoCoverage))
        .count();
    if unreached > 0 {
        result.notes.push(format!(
            "{unreached} mutants no test reached under the test filter ({}); a filter on one class or file misses tests in sibling classes (OrderSenderTests next to OrderTests): widen it to the namespace before writing tests for them",
            options.tests.filters.join(", ")
        ));
    }
}

/// Remembers how long a run that tested everything itself took; a run that
/// took everything from the cache reports those timings instead of zeros.
fn timings(
    targets: &[Target],
    options: &Options,
    cache: &mut cache::Cache,
    result: &mut RunResult,
) {
    let Some(key) = cache::run_key(targets, &options.root) else {
        return;
    };
    let ms = |d: Duration| d.as_millis() as u64;
    if result.sandboxes.is_empty() {
        let Some(earlier) = cache.timings(&key).filter(|_| !result.cached.is_empty()) else {
            return;
        };
        result.prepare = Duration::from_millis(earlier.prepare_ms);
        result.build = Duration::from_millis(earlier.build_ms);
        result.baseline = Duration::from_millis(earlier.baseline_ms);
        result.mutants = Duration::from_millis(earlier.mutants_ms);
        result.test_runs = earlier.test_runs;
        result.timings_from = Some(earlier.at);
        result.notes.push(format!(
            "every result came from the cache; the timings are those of the run that tested them ({} UTC)",
            crate::history::utc(earlier.at)
        ));
    } else if result.cached.is_empty() && !result.budget_hit() {
        cache.remember_timings(
            key,
            cache::Timings {
                prepare_ms: ms(result.prepare),
                build_ms: ms(result.build),
                baseline_ms: ms(result.baseline),
                mutants_ms: ms(result.mutants),
                test_runs: result.test_runs,
                at: crate::report::now(),
            },
        );
    }
}

/// The TypeScript/JavaScript targets that have a Vitest or Jest package
/// above them. The others cannot be tested: their mutants are left untested,
/// with a note per file, and the rest of the run goes on.
fn with_runner<'a>(
    pending: Vec<&'a Target>,
    options: &Options,
    result: &mut RunResult,
) -> Vec<&'a Target> {
    let root = crate::discover::absolute(&options.root);
    let (kept, orphans): (Vec<&Target>, Vec<&Target>) = pending
        .into_iter()
        .partition(|t| node::package_of(&crate::discover::absolute(&t.path), &root).is_some());
    let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for mutant in orphans
        .iter()
        .flat_map(|t| t.mutants.iter().map(move |m| (t.file.as_str(), m)))
        .filter(|(_, m)| !result.outcomes.contains_key(&m.id))
        .map(|(file, m)| {
            *per_file.entry(file).or_default() += 1;
            m
        })
    {
        result.untested.insert(mutant.id, Untested::NoRunner);
    }
    for (file, count) in per_file {
        result.notes.push(format!(
            "{file}: no package.json with vitest or jest above it, so its {count} mutants were not tested"
        ));
    }
    kept
}

/// Mutants turned off in the code or the settings: never tested.
fn ignored(targets: &[Target]) -> impl Iterator<Item = (usize, Outcome)> + '_ {
    targets
        .iter()
        .flat_map(|t| &t.mutants)
        .filter(|m| m.ignored.is_some())
        .map(|m| (m.id, Outcome::Ignored))
}

/// Whether mutants in this file can be run: C#, TypeScript or JavaScript.
pub fn runnable(path: &std::path::Path) -> bool {
    sandbox::Kind::of(path).is_some()
}

/// Gives every mutant an id that is unique across files.
fn number(targets: &mut [Target]) {
    for (id, mutant) in targets
        .iter_mut()
        .flat_map(|t| t.mutants.iter_mut())
        .enumerate()
    {
        mutant.id = id;
    }
}

/// A prepared sandbox and the engine that builds and tests it.
struct Prepared {
    sandbox: sandbox::Sandbox,
    engine: Box<dyn Engine>,
    files: Vec<FileMutants>,
}

fn prepare(
    scope: &Scope,
    roots: &[PathBuf],
    targets: &[&Target],
    options: &Options,
    outcomes: &HashMap<usize, Outcome>,
) -> Result<Prepared, String> {
    let files = sandbox::group(targets, &options.root, outcomes)?;
    progress(&format!(
        "bringing the sandbox up to date ({} files with mutants, {} folders)",
        files.len(),
        roots.len()
    ));
    let sandbox = sandbox::prepare(scope.kind, &scope.root, roots, &files)?;
    let engine: Box<dyn Engine> = match scope.kind {
        sandbox::Kind::CSharp => Box::new(dotnet::Dotnet::prepare(
            &sandbox.dir,
            &files,
            &scope.tests,
            scope.build,
            build_args(options),
        )?),
        sandbox::Kind::Node => Box::new(node::NodeTests::prepare(&sandbox.dir, &files)?),
    };
    Ok(Prepared {
        sandbox,
        engine,
        files,
    })
}

/// `dotnet build` arguments from the options.
fn build_args(options: &Options) -> Vec<String> {
    let mut args: Vec<String> = options
        .build_jobs
        .map(|n| format!("-m:{n}"))
        .into_iter()
        .collect();
    if options.low_priority {
        args.push("-lowPriority:true".into());
    }
    args
}

/// Builds, records reach and tests the open mutants of one language; returns
/// the source files of the tests that killed each mutant.
fn run_language(
    scope: &Scope,
    targets: &[&Target],
    options: &Options,
    result: &mut RunResult,
) -> Result<cache::KilledBy, String> {
    let started = Instant::now();
    let budget = options.budget;
    let mut prepared = prepare(scope, &scope.roots, targets, options, &result.outcomes)?;
    result.prepare += started.elapsed();
    let built = Instant::now();
    progress(&format!(
        "building the sandbox (preparing it took {:.1} s)",
        secs(started)
    ));
    let mut status = build::build(&mut prepared, scope.kind, &mut result.outcomes, budget);
    if status.is_err() && scope.roots.as_slice() != std::slice::from_ref(&scope.root) {
        result.notes.push("the sandbox with only the tested projects did not build, so the whole repository was copied; set `sandbox = \"full\"` in [mutate] to skip the first attempt".into());
        drop(prepared);
        prepared = prepare(
            scope,
            std::slice::from_ref(&scope.root),
            targets,
            options,
            &result.outcomes,
        )?;
        status = build::build(&mut prepared, scope.kind, &mut result.outcomes, budget);
    }
    result.build += built.elapsed();
    result.sandboxes.push(prepared.sandbox.dir.clone());
    result.test_projects.extend(prepared.engine.test_projects());
    if status? == build::Built::OutOfTime {
        mark_untested(&prepared.files, result, Untested::Build);
        return Ok(cache::KilledBy::new());
    }
    progress(&format!(
        "running the tests once to see which test reaches which mutant (the build took {:.1} s; {:.1} s so far)",
        secs(built),
        secs(started)
    ));
    let Some(mut reach) = prepared
        .engine
        .reach(budget)
        .map_err(|err| with_test_hint(err, scope))?
    else {
        mark_untested(&prepared.files, result, Untested::Baseline);
        return Ok(cache::KilledBy::new());
    };
    result.baseline += reach.duration;
    progress(&format!(
        "the tests took {:.1} s (test projects run side by side); by test home: {}",
        reach.duration.as_secs_f64(),
        home_times(&reach)
    ));
    note_idle_projects(scope, &reach, options, result);
    note_empty_filters(scope, &reach, result);
    note_failing(&reach, result);
    reach.set_aside_failing();
    let tested = Instant::now();
    let candidates: Vec<usize> = prepared
        .files
        .iter()
        .flat_map(|f| f.mutants.iter().map(|m| m.id))
        .collect();
    let killers = test_mutants(
        prepared.engine.as_ref(),
        &candidates,
        &reach,
        options,
        result,
    )?;
    result.mutants += tested.elapsed();
    mark_untested(&prepared.files, result, Untested::TestRuns);
    Ok(killer_files(killers, &reach))
}

/// Each test home's (project or file) time in the baseline run, slowest
/// first: its tests plus its startup. Shows which project a long run waits on.
fn home_times(reach: &Reach) -> String {
    let mut by_home: HashMap<&str, Duration> = HashMap::new();
    for (test, time) in &reach.times {
        if let Some(home) = reach.home.get(test) {
            *by_home.entry(home.as_str()).or_default() += *time;
        }
    }
    for (home, startup) in &reach.startup {
        *by_home.entry(home.as_str()).or_default() += *startup;
    }
    let mut homes: Vec<(&str, Duration)> = by_home.into_iter().collect();
    homes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let shown: Vec<String> = homes
        .iter()
        .take(8)
        .map(|(home, time)| format!("{home} {:.1} s", time.as_secs_f64()))
        .collect();
    let more = homes.len().saturating_sub(shown.len());
    let tail = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    format!("{}{tail}", shown.join(", "))
}

fn note_failing(reach: &Reach, result: &mut RunResult) {
    if reach.failing.is_empty() {
        return;
    }
    let names: Vec<&String> = reach.failing.iter().take(5).collect();
    tellln!(
        "note: {} tests fail without any mutant and are left out: {names:?}",
        reach.failing.len()
    );
    result.set_aside.extend(reach.failing.iter().cloned());
}

/// Every mutant of `files` without an outcome was left untested at `step`.
fn mark_untested(files: &[FileMutants], result: &mut RunResult, step: Untested) {
    for mutant in files.iter().flat_map(|f| &f.mutants) {
        if !result.outcomes.contains_key(&mutant.id) {
            result.untested.entry(mutant.id).or_insert(step);
        }
    }
}

/// The repository files of each mutant's killing tests; empty when a
/// killing test's file is not known, so the cache relies on every test.
fn killer_files(killers: HashMap<usize, Vec<String>>, reach: &Reach) -> cache::KilledBy {
    killers
        .into_iter()
        .map(|(id, tests)| {
            let files: Option<Vec<&Vec<String>>> =
                tests.iter().map(|t| reach.files.get(t)).collect();
            let unique: BTreeSet<String> = files.into_iter().flatten().flatten().cloned().collect();
            (id, unique.into_iter().collect())
        })
        .collect()
}

/// One line on stderr, so a long run shows where it is.
fn progress(step: &str) {
    tellln!("  {step}...");
}

fn secs(since: Instant) -> f64 {
    since.elapsed().as_secs_f64()
}

/// FNV-1a, 64 bit: stable across Rust versions and machines, unlike std's hasher.
pub fn fnv(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.iter().chain(&[0xff]) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// A run that the budget stopped or never started.
enum Ran<T> {
    Done(T),
    Skipped,
}

/// Tests every candidate: packs first, then solo runs for whatever packs
/// could not decide. Returns each killed mutant's killing tests.
fn test_mutants(
    engine: &dyn Engine,
    candidates: &[usize],
    reach: &Reach,
    options: &Options,
    result: &mut RunResult,
) -> Result<HashMap<usize, Vec<String>>, String> {
    let (reached, unreached): (Vec<usize>, Vec<usize>) = candidates
        .iter()
        .partition(|id| reach.by_mutant.contains_key(id) || reach.outside.contains(id));
    for id in unreached {
        result.outcomes.insert(id, Outcome::NoCoverage);
    }
    record_reach(&reached, reach, result);
    let mut killers = HashMap::new();
    let mut solo = Vec::new();
    if options.no_mix {
        solo = reached.clone();
    } else {
        // Most mutants fail their first test: each runs with one test first,
        // and only the survivors with the rest.
        let first = first_tests(&reached, reach);
        progress(&format!(
            "{} mutants are reached by tests: first one test each, {} at a time",
            reached.len(),
            options.jobs
        ));
        let survivors = run_packs(
            engine,
            &reached,
            &first,
            options,
            result,
            &mut killers,
            &mut solo,
        )?;
        let rest = other_tests(&survivors, reach, &first);
        let more: Vec<usize> = survivors
            .into_iter()
            .filter(|id| rest.by_mutant.contains_key(id))
            .collect();
        if !more.is_empty() {
            progress(&format!("{} survivors: their other tests", more.len()));
            run_packs(
                engine,
                &more,
                &rest,
                options,
                result,
                &mut killers,
                &mut solo,
            )?;
        }
    }
    let runner = Runner::new(engine, reach, options)?;
    for (id, run) in runner.solos(&solo) {
        if let Ran::Done(run) = run? {
            let expected = reach.by_mutant.get(&id);
            let (outcome, killed_by) = solo_outcome(&run, &reach.failing, expected);
            if outcome == Outcome::Killed && crashed(run.0, &run.1, expected) {
                result.crash_kills.insert(id);
            }
            result.outcomes.insert(id, outcome);
            killers.insert(id, killed_by);
        }
    }
    let (started, again) = runner.finish();
    result.test_runs += started;
    result.retries += again;
    Ok(killers)
}

/// Records which tests reach each mutant, then packs them (or, with
/// `--no-mix`, runs each alone).
fn record_reach(reached: &[usize], reach: &Reach, result: &mut RunResult) {
    for id in reached {
        result.reached.insert(*id, reaching_tests(reach, *id));
        result.reached_files.insert(*id, reaching_files(reach, *id));
    }
}

/// Packs `mutants` with the tests `reach` gives them and runs the packs;
/// mutants a pack could not decide go to `solo`. Returns the survivors.
fn run_packs(
    engine: &dyn Engine,
    mutants: &[usize],
    reach: &Reach,
    options: &Options,
    result: &mut RunResult,
    killers: &mut HashMap<usize, Vec<String>>,
    solo: &mut Vec<usize>,
) -> Result<Vec<usize>, String> {
    let (packs, outside) = pack(mutants, reach, PACK_TESTS);
    solo.extend(outside);
    let runner = Runner::new(engine, reach, options)?;
    for (pack, run) in packs.iter().zip(runner.packs(&packs)) {
        if let Ran::Done(run) = run? {
            solo.extend(resolve(pack, reach, &run, &mut result.outcomes, killers));
        }
    }
    let (started, again) = runner.finish();
    result.test_runs += started;
    result.retries += again;
    Ok(mutants
        .iter()
        .copied()
        .filter(|id| result.outcomes.get(id) == Some(&Outcome::Survived))
        .collect())
}

/// One test per mutant: the cheapest of those that reach it, where a test
/// costs its time plus half a second for every mutant it already carries
/// (each one needs a pack of its own).
fn first_tests(mutants: &[usize], reach: &Reach) -> Reach {
    let mut order: Vec<usize> = mutants
        .iter()
        .copied()
        .filter(|id| reach.by_mutant.contains_key(id))
        .collect();
    order.sort_by_key(|id| (reach.by_mutant[id].len(), *id));
    let mut load: HashMap<&String, u32> = HashMap::new();
    let mut by_mutant = HashMap::new();
    for id in order {
        let cost = |t: &&String| {
            let time = reach.times.get(*t).copied().unwrap_or_default();
            time + Duration::from_millis(500) * load.get(t).copied().unwrap_or(0)
        };
        let Some(test) = reach.by_mutant[&id].iter().min_by_key(cost) else {
            continue;
        };
        *load.entry(test).or_default() += 1;
        by_mutant.insert(id, BTreeSet::from([test.clone()]));
    }
    Reach {
        by_mutant,
        ..reach.clone()
    }
}

/// The tests the first round did not run, for the mutants that survived it.
fn other_tests(survivors: &[usize], reach: &Reach, first: &Reach) -> Reach {
    let by_mutant = survivors
        .iter()
        .filter_map(|id| {
            let tried = first.by_mutant.get(id)?;
            let rest: BTreeSet<String> = reach
                .by_mutant
                .get(id)?
                .difference(tried)
                .cloned()
                .collect();
            (!rest.is_empty()).then_some((*id, rest))
        })
        .collect();
    Reach {
        by_mutant,
        outside: HashSet::new(),
        ..reach.clone()
    }
}

/// Up to three tests that reach `mutant`, with their files: where a test
/// that notices it would go.
fn reaching_tests(reach: &Reach, mutant: usize) -> Vec<String> {
    let mut found: Vec<String> = reach
        .by_mutant
        .get(&mutant)
        .into_iter()
        .flatten()
        .take(3)
        .map(|test| match reach.files.get(test).and_then(|f| f.first()) {
            Some(file) => format!("{test} ({file})"),
            None => test.clone(),
        })
        .collect();
    if reach.outside.contains(&mutant) {
        found.push(OUTSIDE.to_string());
    }
    found
}

/// What reaches a mutant that runs outside any test.
const OUTSIDE: &str =
    "setup outside any test (static constructor, field initializer or module load), so every test runs it";

/// The files of the tests that reach `mutant`.
fn reaching_files(reach: &Reach, mutant: usize) -> BTreeSet<String> {
    reach
        .by_mutant
        .get(&mutant)
        .into_iter()
        .flatten()
        .filter_map(|test| reach.files.get(test))
        .flatten()
        .cloned()
        .collect()
}

/// Runs packs and solo runs in parallel, within the budget.
struct Runner<'a> {
    engine: &'a dyn Engine,
    reach: &'a Reach,
    budget: Budget,
    timeouts: Timeouts,
    pool: rayon::ThreadPool,
    done: std::sync::atomic::AtomicUsize,
    again: std::sync::atomic::AtomicUsize,
}

impl<'a> Runner<'a> {
    fn new(
        engine: &'a dyn Engine,
        reach: &'a Reach,
        options: &Options,
    ) -> Result<Runner<'a>, String> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(options.jobs.max(1))
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Runner {
            engine,
            reach,
            budget: options.budget,
            timeouts: options.timeouts,
            pool,
            done: std::sync::atomic::AtomicUsize::new(0),
            again: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn packs(&self, packs: &[Pack]) -> Vec<Result<Ran<PackRun>, String>> {
        self.pool.install(|| {
            packs
                .par_iter()
                .enumerate()
                .map(|(i, p)| {
                    let tests: BTreeSet<String> = p.plan.iter().map(|(t, _)| t.clone()).collect();
                    self.within_budget(
                        self.timeouts.limit(self.reach, Some(&tests)),
                        |limit| self.engine.pack(p, self.reach, limit, &format!("p{i}")),
                        |run| run.is_none(),
                    )
                })
                .collect()
        })
    }

    fn solos(&self, ids: &[usize]) -> Vec<(usize, Result<Ran<SoloRun>, String>)> {
        self.pool.install(|| {
            ids.par_iter()
                .map(|&id| {
                    let tests = (!self.reach.outside.contains(&id))
                        .then(|| self.reach.by_mutant.get(&id))
                        .flatten();
                    let limit = self.timeouts.limit(self.reach, tests);
                    let solo = |limit: Duration, tag: &str| {
                        self.within_budget(
                            limit,
                            |limit| self.engine.solo(id, self.reach, limit, tag),
                            |run| run.0 == TestRun::TimedOut,
                        )
                    };
                    let mut run = solo(limit, &format!("s{id}"));
                    // A busy machine makes slow runs, not endless ones: a
                    // timeout counts only when a run with four times the time
                    // (and at least 30 s more) also times out.
                    if matches!(&run, Ok(Ran::Done((TestRun::TimedOut, _)))) {
                        let longer = (limit * 4).max(limit + Duration::from_secs(30));
                        self.again
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        run = solo(longer, &format!("s{id}-again"));
                    }
                    (id, run)
                })
                .collect()
        })
    }

    /// Starts `run` only while there is budget left, and drops its result
    /// when the budget, not the mutant, stopped it.
    fn within_budget<T>(
        &self,
        limit: Duration,
        run: impl FnOnce(Duration) -> Result<T, String>,
        timed_out: impl Fn(&T) -> bool,
    ) -> Result<Ran<T>, String> {
        if self.budget.spent() {
            return Ok(Ran::Skipped);
        }
        let ran = run(self.budget.limit(limit))?;
        let n = self.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        tell!("\r  test runs finished: {n}");
        if timed_out(&ran) && self.budget.spent() {
            return Ok(Ran::Skipped);
        }
        Ok(Ran::Done(ran))
    }

    /// Ends the progress line; returns how many runs were started and how
    /// many of them were second runs after a timeout.
    fn finish(self) -> (usize, usize) {
        let started = self.done.into_inner();
        if started > 0 {
            tellln!();
        }
        (started, self.again.into_inner())
    }
}

/// Packs mutants so that within a pack no test carries two mutants, filling
/// the biggest mutants first. Mutants reached outside tests run solo.
pub fn pack(mutants: &[usize], reach: &Reach, max_tests: usize) -> (Vec<Pack>, Vec<usize>) {
    let mut ordered: Vec<usize> = mutants
        .iter()
        .copied()
        .filter(|id| !reach.outside.contains(id))
        .collect();
    ordered.sort_by_key(|id| std::cmp::Reverse(reach.by_mutant.get(id).map_or(0, BTreeSet::len)));
    let mut packs: Vec<Pack> = Vec::new();
    for id in ordered {
        let tests = &reach.by_mutant[&id];
        let fits = |p: &&mut Pack| {
            p.used.len() + tests.len() <= max_tests && tests.iter().all(|t| !p.used.contains(t))
        };
        let index = match packs.iter_mut().position(|p| fits(&p)) {
            Some(index) => index,
            None => {
                packs.push(Pack::default());
                packs.len() - 1
            }
        };
        place(&mut packs[index], id, tests);
    }
    let solo = mutants
        .iter()
        .copied()
        .filter(|id| reach.outside.contains(id))
        .collect();
    (packs, solo)
}

fn place(pack: &mut Pack, id: usize, tests: &BTreeSet<String>) {
    pack.mutants.push(id);
    for test in tests {
        pack.used.insert(test.clone());
        pack.plan.push((test.clone(), id));
    }
}

/// Records the mutants a pack decided, with their killing tests; returns the
/// ones it could not (a test did not report, or the run timed out).
fn resolve(
    pack: &Pack,
    reach: &Reach,
    run: &PackRun,
    outcomes: &mut HashMap<usize, Outcome>,
    killers: &mut HashMap<usize, Vec<String>>,
) -> Vec<usize> {
    let Some(results) = run else {
        return pack.mutants.clone();
    };
    let mut undecided = Vec::new();
    for &id in &pack.mutants {
        let tests = &reach.by_mutant[&id];
        if tests.iter().any(|t| !results.contains_key(t)) {
            undecided.push(id);
            continue;
        }
        let failed: Vec<String> = tests.iter().filter(|t| !results[*t]).cloned().collect();
        let outcome = if failed.is_empty() {
            Outcome::Survived
        } else {
            Outcome::Killed
        };
        outcomes.insert(id, outcome);
        killers.insert(id, failed);
    }
    undecided
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reach(entries: &[(usize, &[&str])]) -> Reach {
        Reach {
            by_mutant: entries
                .iter()
                .map(|(id, tests)| (*id, tests.iter().map(|t| t.to_string()).collect()))
                .collect(),
            ..Reach::default()
        }
    }

    /// Options for a run in `root`, with every default.
    fn options(root: &std::path::Path) -> Options {
        Options {
            root: root.to_path_buf(),
            jobs: 1,
            use_cache: false,
            cache_dir: root.join(".cache"),
            tests: TestSelection::default(),
            sandbox: SandboxMode::Closure,
            budget: Budget::default(),
            timeouts: Timeouts::default(),
            build_jobs: None,
            low_priority: false,
            rerun_survivors: false,
            no_mix: false,
            cached_only: false,
        }
    }

    fn set(tests: &[&str]) -> BTreeSet<String> {
        tests.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn tests_failing_without_mutants_decide_nothing() {
        let failing = set(&["Broken"]);
        let results = |broken: bool, other: bool| -> SoloRun {
            let tests = [("Broken".to_string(), broken), ("Other".to_string(), other)];
            (TestRun::Failed, tests.into())
        };
        assert_eq!(
            solo_outcome(&results(false, true), &failing, None).0,
            Outcome::Survived
        );
        assert_eq!(
            solo_outcome(&results(false, false), &failing, None),
            (Outcome::Killed, vec!["Other".to_string()])
        );

        let mut reach = reach(&[(0, &["Broken"]), (1, &["Broken", "Other"])]);
        reach.failing = failing;
        reach.set_aside_failing();
        assert!(!reach.by_mutant.contains_key(&0));
        assert_eq!(reach.by_mutant[&1].len(), 1);
    }

    #[test]
    fn a_run_that_crashed_before_its_tests_reported_is_a_runtime_error() {
        let none = BTreeSet::new();
        assert_eq!(
            solo_outcome(&(TestRun::Failed, HashMap::new()), &none, None).0,
            Outcome::RuntimeError
        );
        let expected = set(&["A", "B"]);
        let partial: SoloRun = (TestRun::Failed, [("A".to_string(), true)].into());
        assert_eq!(
            solo_outcome(&partial, &none, Some(&expected)).0,
            Outcome::RuntimeError
        );
        let timed_out: SoloRun = (TestRun::TimedOut, HashMap::new());
        assert_eq!(
            solo_outcome(&timed_out, &none, Some(&expected)).0,
            Outcome::Timeout
        );
        assert!(!Outcome::RuntimeError.counts() && Outcome::NoCoverage.counts());
    }

    #[test]
    fn mutants_with_different_tests_share_a_pack() {
        let reach = reach(&[
            (0, &["A.one", "A.two"]),
            (1, &["B.one"]),
            (2, &["A.one"]),
            (3, &["C.one"]),
        ]);
        let (packs, solo) = pack(&[0, 1, 2, 3], &reach, 100);
        assert!(solo.is_empty());
        // 0 and 2 both need A.one, so they cannot share; 1 and 3 fit with either.
        assert_eq!(packs.len(), 2);
        assert_eq!(packs[0].mutants, [0, 1, 3]);
        assert_eq!(packs[1].mutants, [2]);
        assert!(packs[0].plan.contains(&("B.one".to_string(), 1)));
    }

    #[test]
    fn a_baseline_with_half_the_tests_failing_stops_the_run() {
        let outcome = |passed: &[bool]| -> HashMap<String, bool> {
            passed
                .iter()
                .enumerate()
                .map(|(i, p)| (format!("T{i}"), *p))
                .collect()
        };
        assert!(check_baseline(TestRun::Failed, &outcome(&[true, true, false])).is_ok());
        let half = check_baseline(TestRun::Failed, &outcome(&[true, false])).unwrap_err();
        assert!(half.contains("1 of 2 tests fail"), "{half}");
        assert!(check_baseline(TestRun::Passed, &HashMap::new()).is_err());
    }

    #[test]
    fn the_first_round_gives_each_mutant_one_cheap_test_and_spreads_the_load() {
        let mut reach = reach(&[
            (0, &["Fast", "Slow"]),
            (1, &["Fast", "Slow"]),
            (2, &["Fast"]),
            (3, &["Slow", "Other"]),
        ]);
        reach.times.insert("Fast".into(), Duration::from_millis(10));
        reach.times.insert("Slow".into(), Duration::from_secs(30));
        reach
            .times
            .insert("Other".into(), Duration::from_millis(20));
        let first = first_tests(&[0, 1, 2, 3], &reach);
        let one = |id: usize| first.by_mutant[&id].iter().next().unwrap().clone();
        // Mutant 2 has only Fast; 0 and 1 share Fast too (half a second per
        // mutant is far less than the slow test); 3 takes Other.
        assert!(first.by_mutant.values().all(|tests| tests.len() == 1));
        assert_eq!(one(2), "Fast");
        assert_eq!(one(0), "Fast");
        assert_eq!(one(3), "Other");
        let rest = other_tests(&[0, 2, 3], &reach, &first);
        assert_eq!(rest.by_mutant[&0], BTreeSet::from(["Slow".to_string()]));
        assert!(!rest.by_mutant.contains_key(&2), "nothing left for 2");
        assert_eq!(rest.by_mutant[&3], BTreeSet::from(["Slow".to_string()]));
    }

    #[test]
    fn packs_respect_the_test_limit_and_outside_reach_runs_solo() {
        let mut reach = reach(&[(0, &["A"]), (1, &["B"]), (2, &["C"])]);
        reach.outside.insert(2);
        let (packs, solo) = pack(&[0, 1, 2], &reach, 1);
        assert_eq!(packs.len(), 2);
        assert_eq!(solo, [2]);
    }

    #[test]
    fn resolving_a_pack_uses_each_mutants_own_tests() {
        let reach = reach(&[(0, &["A", "B"]), (1, &["C"]), (2, &["D"])]);
        let (packs, _) = pack(&[0, 1, 2], &reach, 100);
        let results: HashMap<String, bool> = [("A", true), ("B", false), ("C", true)]
            .iter()
            .map(|(t, p)| (t.to_string(), *p))
            .collect();
        let mut outcomes = HashMap::new();
        let mut killers = HashMap::new();
        let undecided = resolve(
            &packs[0],
            &reach,
            &Some(results),
            &mut outcomes,
            &mut killers,
        );
        assert_eq!(outcomes[&0], Outcome::Killed);
        assert_eq!(killers[&0], ["B"]);
        assert_eq!(outcomes[&1], Outcome::Survived);
        assert_eq!(undecided, [2]);
        assert_eq!(
            resolve(&packs[0], &reach, &None, &mut outcomes, &mut killers).len(),
            3
        );
    }

    #[test]
    fn a_run_may_take_its_startup_and_tests_times_the_factor_plus_extra() {
        let mut reach = reach(&[(0, &["A.one", "A.two"]), (1, &["B.one"])]);
        for (test, home, ms) in [
            ("A.one", "A", 100),
            ("A.two", "A", 300),
            ("B.one", "B", 1000),
        ] {
            reach.home.insert(test.into(), home.into());
            reach.times.insert(test.into(), Duration::from_millis(ms));
        }
        reach
            .startup
            .insert("A".into(), Duration::from_millis(2000));
        reach
            .startup
            .insert("B".into(), Duration::from_millis(4000));
        let timeouts = Timeouts::default();
        let one: BTreeSet<String> = ["A.one".to_string()].into();
        // (2000 + 100) × 1.5 + 5000
        assert_eq!(
            timeouts.limit(&reach, Some(&one)),
            Duration::from_millis(8150)
        );
        // Every test: (2000 + 4000 + 1400) × 1.5 + 5000
        assert_eq!(timeouts.limit(&reach, None), Duration::from_millis(16100));
    }

    #[test]
    fn the_budget_shortens_limits_and_runs_out() {
        let none = Budget::minutes(None);
        assert!(!none.spent());
        assert_eq!(none.limit(Duration::from_secs(5)), Duration::from_secs(5));
        let short = Budget::minutes(Some(0.5));
        assert!(short.limit(Duration::from_secs(3600)) <= Duration::from_secs(30));
        assert!(Budget::minutes(Some(0.0)).spent());
    }

    #[test]
    fn killers_map_to_their_files_only_when_every_file_is_known() {
        let mut reach = Reach::default();
        reach.files.insert("T.a".into(), vec!["tests/A.cs".into()]);
        let killers = HashMap::from([
            (1, vec!["T.a".to_string()]),
            (2, vec!["T.a".to_string(), "T.b".to_string()]),
        ]);
        let files = killer_files(killers, &reach);
        assert_eq!(files[&1], ["tests/A.cs"]);
        assert!(files[&2].is_empty());
    }

    #[test]
    fn files_without_a_test_runner_are_left_untested_and_the_rest_go_on() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("web/src")).unwrap();
        std::fs::create_dir_all(root.join("tools")).unwrap();
        std::fs::write(
            root.join("web/package.json"),
            r#"{"devDependencies":{"vitest":"3"}}"#,
        )
        .unwrap();
        let source = "export function f(a: number) {\n  return a > 1;\n}\n";
        std::fs::write(root.join("web/src/a.ts"), source).unwrap();
        std::fs::write(root.join("tools/b.mjs"), source).unwrap();
        let wanted = |file: &str| crate::mutate::Wanted {
            file: file.into(),
            path: root.join(file),
            name: "f".into(),
            start_line: 1,
            end_line: 3,
            complexity: 2,
            reason: "selected",
            lines: None,
        };
        let (mut targets, _) = crate::mutate::targets(
            vec![wanted("web/src/a.ts"), wanted("tools/b.mjs")],
            &dog_food_core::mutate::Settings::default(),
        )
        .unwrap();
        number(&mut targets);
        let options = options(root);
        let mut result = RunResult::default();
        let kept = with_runner(targets.iter().collect(), &options, &mut result);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].file, "web/src/a.ts");
        let orphans = targets[1].mutants.len();
        assert!(orphans > 0);
        assert_eq!(result.untested.len(), orphans);
        assert!(result.untested.values().all(|s| *s == Untested::NoRunner));
        assert!(!result.budget_hit());
        assert_eq!(
            result.notes,
            [format!("tools/b.mjs: no package.json with vitest or jest above it, so its {orphans} mutants were not tested")]
        );
    }

    #[test]
    fn a_filter_without_a_project_names_the_projects_that_ran_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut scope = Scope::fixed(sandbox::Kind::CSharp, dir.path(), 1, 1);
        scope.tests = [
            "Shop.Workflows.Tests",
            "Shop.Plans.Tests",
            "Shop.Coaches.Tests",
        ]
        .iter()
        .map(|name| projects::TestProject {
            path: PathBuf::from(format!("{name}/{name}.csproj")),
            assembly: name.to_string(),
            filter: Some(name.to_string()),
        })
        .collect();
        let mut reach = Reach::default();
        reach
            .home
            .insert("W.one".into(), "Shop.Workflows.Tests".into());
        let mut options = options(dir.path());
        options.tests.filters = vec!["Shop.Workflows.Tests".into()];
        let mut result = RunResult::default();
        note_idle_projects(&scope, &reach, &options, &mut result);
        let note = result.notes.join("\n");
        assert!(note.contains("ran no test under it and only cost a start each: Shop.Plans.Tests, Shop.Coaches.Tests"), "{note}");
        assert!(
            note.contains(r#"--test-filter "Shop.Workflows.Tests:FullyQualifiedName~.""#),
            "{note}"
        );
        // A filter for one project says nothing.
        options.tests.filters = vec!["Shop.Workflows.Tests:Category=Fast".into()];
        let mut quiet = RunResult::default();
        note_idle_projects(&scope, &reach, &options, &mut quiet);
        assert!(quiet.notes.is_empty());
    }

    #[test]
    fn a_project_whose_filter_ran_nothing_is_named_and_an_empty_baseline_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let mut scope = Scope::fixed(sandbox::Kind::CSharp, dir.path(), 1, 1);
        scope.tests = vec![projects::TestProject {
            path: PathBuf::from("Api.Tests/Api.Tests.csproj"),
            assembly: "Api.Tests".into(),
            filter: Some("FullyQualifiedName~MembersTests".into()),
        }];
        scope.left_out = vec!["Api.IntegrationTests".into()];
        let mut result = RunResult::default();
        note_empty_filters(&scope, &Reach::default(), &mut result);
        assert!(
            result.notes[0].contains(
                "ran no test under their filter: Api.Tests (FullyQualifiedName~MembersTests)"
            ),
            "{:?}",
            result.notes
        );
        let hint = with_test_hint("the tests did not run without mutants".into(), &scope);
        assert!(hint.contains("did not run: Api.IntegrationTests"), "{hint}");
        assert!(
            hint.contains("Api.Tests: FullyQualifiedName~MembersTests"),
            "{hint}"
        );
    }

    #[test]
    fn baseline_times_add_up_per_test_home_slowest_first() {
        let mut reach = Reach::default();
        for (test, home, ms) in [
            ("A.one", "Fast.Tests", 100),
            ("B.one", "Slow.Tests", 900),
            ("B.two", "Slow.Tests", 600),
        ] {
            reach.home.insert(test.into(), home.into());
            reach.times.insert(test.into(), Duration::from_millis(ms));
        }
        reach
            .startup
            .insert("Fast.Tests".into(), Duration::from_millis(400));
        assert_eq!(home_times(&reach), "Slow.Tests 1.5 s, Fast.Tests 0.5 s");
    }

    #[test]
    fn fnv_is_stable_and_separates_parts() {
        assert_eq!(fnv(&[b"abc"]), fnv(&[b"abc"]));
        assert_ne!(fnv(&[b"ab", b"c"]), fnv(&[b"a", b"bc"]));
    }
}
