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
mod check;
mod dotnet;
mod exec;
pub mod log;
mod node;
pub mod priority;
pub mod projects;
mod sandbox;
mod scope;
mod warm;
mod worker;

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
    /// The run of its language failed (the error is in `RunResult::failed`).
    Failed,
    /// Its own test run could not start or be set up (a run folder that
    /// could not be cleared, a worker that would not start); the error is
    /// in `RunResult::run_errors`. Not the budget's doing.
    RunError,
    /// No test that ran reaches it, and a test project left out of the run
    /// (it did not build or could not run) uses its code.
    SkippedProject,
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
    /// Survived without a run: tests reach it, but in the run without
    /// mutants its value never differed from the original's in any of them.
    pub unchanged: BTreeSet<usize>,
    /// Runtime errors: what the run's output said as it ended.
    pub crash_reasons: HashMap<usize, String>,
    /// Mutants whose test run could not start, and the error.
    pub run_errors: HashMap<usize, String>,
    /// Mutants left undecided with some tests run: (tests that ran and
    /// passed, tests that reach it). A mutant cut short by the budget or a
    /// failed run is not a survivor; this is the evidence it has.
    pub partial: HashMap<usize, (usize, usize)>,
    /// Languages whose run failed, and why; the others' results still count.
    pub failed: Vec<String>,
    /// Things the reader should know, such as a fallback to a full sandbox.
    pub notes: Vec<String>,
    /// Per tested mutant, up to three tests that reach it ("name (file)").
    pub reached: HashMap<usize, Vec<String>>,
    /// Per tested mutant, the files of every test that reaches it.
    pub reached_files: HashMap<usize, BTreeSet<String>>,
    /// With `--rerun-survivors`: the survivors the cache had that this run
    /// tested again.
    pub retested: BTreeSet<usize>,
    pub jobs: usize,
    /// When every result came from the cache: when the run whose timings
    /// are reported ended (seconds since 1970).
    pub timings_from: Option<u64>,
}

impl RunResult {
    /// Whether `--max-minutes` stopped the run.
    pub fn budget_hit(&self) -> bool {
        self.untested.values().any(|step| step.by_budget())
    }
}

impl Untested {
    /// Whether the budget stopped this mutant (as against something that
    /// could not be tested however long the run had).
    pub fn by_budget(self) -> bool {
        !matches!(
            self,
            Untested::NoRunner
                | Untested::NotCached
                | Untested::Failed
                | Untested::RunError
                | Untested::SkippedProject
        )
    }
}

/// Which tests reach which mutant, from the run with no mutant active.
#[derive(Debug, Default, Clone)]
pub struct Reach {
    pub by_mutant: HashMap<usize, BTreeSet<String>>,
    /// Mutants reached outside any test (static setup, module load): they run
    /// with every test.
    pub outside: HashSet<usize>,
    /// For those reached in code that runs once (a static constructor, a
    /// lazy fill): the tests that code ran in. Their classes go first in the
    /// mutant's own run.
    pub outside_in: HashMap<usize, BTreeSet<String>>,
    /// TypeScript: the test files that loaded a mutant outside any test
    /// (relative to the package): its solo run needs only them.
    pub outside_files: HashMap<usize, BTreeSet<String>>,
    /// Where each test lives: its test project (C#) or test file (TypeScript).
    pub home: HashMap<String, String>,
    /// Each test's source files, relative to the repository, when known.
    pub files: HashMap<String, Vec<String>>,
    /// Tests that fail with no mutant active: left out of every decision.
    pub failing: BTreeSet<String>,
    /// Tests that failed with no mutant active and passed when run again.
    pub flaky: BTreeSet<String>,
    /// Tests that leave state behind for the next test of their home when they
    /// fail (a mock's queued value): they share a run with no other mutated test
    /// of that home.
    pub queuing: BTreeSet<String>,
    pub duration: Duration,
    /// Each test's time in the run without mutants.
    pub times: HashMap<String, Duration>,
    /// Per test home (project or file): what a run of it takes besides its
    /// tests (starting the test host, discovery).
    pub startup: HashMap<String, Duration>,
    /// Mutants whose switch worked out their value next to the original's
    /// in the run without mutants, in every test that reached them
    /// ([`dog_food_core::Mutant::probe`]).
    pub probed: HashSet<usize>,
    /// For those: the tests in which their value differed from the original's.
    pub changed: HashMap<usize, BTreeSet<String>>,
}

impl Reach {
    /// The probed mutants that tests reach but whose value no test changed:
    /// they behave the same in every test, so they survive without a run.
    /// Every other probed mutant keeps only the tests that changed its value,
    /// the only ones that can notice it.
    fn unchanged(&mut self) -> BTreeSet<usize> {
        let mut unchanged = BTreeSet::new();
        for id in &self.probed {
            if self.outside.contains(id) {
                continue;
            }
            let Some(tests) = self.by_mutant.get_mut(id) else {
                continue;
            };
            let changed = self.changed.get(id);
            tests.retain(|t| changed.is_some_and(|c| c.contains(t)));
            if tests.is_empty() {
                self.by_mutant.remove(id);
                unchanged.insert(*id);
            }
        }
        unchanged
    }

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
/// When no test reported, the error quotes what the run's output in
/// `folder` said: a compile or type error is then one read away.
fn check_baseline(
    status: TestRun,
    outcome: &HashMap<String, bool>,
    folder: Option<&std::path::Path>,
) -> Result<(), String> {
    if matches!(status, TestRun::TimedOut | TestRun::Crashed) || outcome.is_empty() {
        return Err(match folder.and_then(exec::crash_reason) {
            Some(said) => {
                format!("the tests did not run without mutants; the run's output said: {said}")
            }
            None => {
                "the tests did not run without mutants; run them once yourself to see why".into()
            }
        });
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
    /// The test host ended before every test it was asked for reported:
    /// the results it did report are partial.
    Crashed,
}

/// Per-test results of a pack: `None` when the whole run timed out.
pub type PackRun = Option<TestResults>;

/// What one test run reported.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TestResults {
    /// Each test → whether it passed.
    pub outcome: HashMap<String, bool>,
    /// Failed tests that fail without their mutant too (the kill check):
    /// failed by state the test process carries, not by the mutant.
    pub polluted: BTreeSet<String>,
    /// The (test, mutant) pairs whose planned mutant took effect in that
    /// test; `None` when the run cannot tell (a runner without the check).
    /// A planned pair not here ran the original code: its pass says nothing.
    pub activated: Option<BTreeSet<(String, usize)>>,
    /// The run stopped at its time limit; these results are what it reported first.
    pub timed_out: bool,
    /// Tests that had started and not reported when it stopped.
    pub running: BTreeSet<String>,
}

impl TestResults {
    pub fn of(outcome: HashMap<String, bool>) -> TestResults {
        TestResults {
            outcome,
            ..TestResults::default()
        }
    }

    /// Whether the run shows that `test` ran mutant `id` mutated: it took
    /// effect, or the run cannot tell.
    fn activated(&self, test: &str, id: usize) -> bool {
        self.activated
            .as_ref()
            .is_none_or(|pairs| pairs.contains(&(test.to_string(), id)))
    }
}

/// A solo run: how the run ended and what it reported.
pub type SoloRun = (TestRun, TestResults);

/// A mutant's verdict from a solo run.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// The outcome, and the tests that killed it.
    Decided(Outcome, Vec<String>),
    /// The run cannot be trusted either way, and a fresh process can: its
    /// only kills are polluted, or its planned mutant never took effect.
    Fresh,
}

/// A mutant's verdict from a solo run. A test that passes without mutants
/// failed: killed, unless the kill check found the failure is the process's,
/// not the mutant's. Tests that fail anyway do not count. A run whose host
/// ended before its tests reported, or that reported no test at all,
/// crashed: a runtime error. A pass whose mutant never took effect in one
/// of its `planned` tests, or a kill the check pinned on the process, needs
/// a fresh process.
fn solo_outcome(
    (status, results): &SoloRun,
    failing: &BTreeSet<String>,
    planned: Option<(&BTreeSet<String>, usize)>,
) -> Verdict {
    if *status == TestRun::TimedOut {
        return Verdict::Decided(Outcome::Timeout, Vec::new());
    }
    let failed: Vec<&String> = results
        .outcome
        .iter()
        .filter(|(test, passed)| !**passed && !failing.contains(*test))
        .map(|(test, _)| test)
        .collect();
    let killers: Vec<String> = failed
        .iter()
        .filter(|t| !results.polluted.contains(**t))
        .map(|t| t.to_string())
        .collect();
    if !killers.is_empty() {
        return Verdict::Decided(Outcome::Killed, killers);
    }
    if !failed.is_empty() {
        return Verdict::Fresh;
    }
    if crashed(*status, results) {
        return Verdict::Decided(Outcome::RuntimeError, Vec::new());
    }
    let inert = planned.is_some_and(|(tests, id)| tests.iter().any(|t| !results.activated(t, id)));
    if inert {
        return Verdict::Fresh;
    }
    Verdict::Decided(Outcome::Survived, Vec::new())
}

/// The run said its host crashed, or no test reported. A failed run with
/// tests missing is not a crash by itself: a worker skips a killed mutant's
/// other tests.
fn crashed(status: TestRun, results: &TestResults) -> bool {
    status == TestRun::Crashed || results.outcome.is_empty()
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
    /// Runs `mutant` alone: with every test that reaches it, in a warm test
    /// process; or, when `fresh` or the mutant is reached outside any test,
    /// in a new process with the mutant on from the start, which stops at
    /// the first failing test.
    fn solo(
        &self,
        mutant: usize,
        reach: &Reach,
        fresh: bool,
        limit: Duration,
        tag: &str,
    ) -> Result<SoloRun, String>;
    fn test_projects(&self) -> Vec<String>;
    /// Compile checks in memory for the mutated files, when this language has
    /// them: the search for mutants that do not compile then takes one build.
    fn checker(&self, _files: &[sandbox::FileMutants]) -> Option<check::Checker> {
        None
    }
    /// Whether a mutant that survived its packs runs once more alone before
    /// it counts: other mutants in a pack can leave state behind that hides
    /// it, and this runner has no check that catches that.
    fn confirms_survivors(&self) -> bool {
        false
    }
    /// Why the run tagged `tag` ended without reporting its tests, from its
    /// output.
    fn crash_reason(&self, _tag: &str) -> Option<String> {
        None
    }
    /// What the reader should know about how the tests ran (a test project
    /// that ran without the worker, and why).
    fn notes(&self) -> Vec<String> {
        Vec::new()
    }
    /// After a build of everything failed with no mutant in the sandbox
    /// (`errors`), leaves out the test projects that do not build, so the
    /// others still run. False when none could be left out: every test
    /// project fails, or the code under test itself does not build.
    fn drop_broken(&mut self, _errors: &[String], _budget: Budget) -> bool {
        false
    }
    /// Whether a test project left out of the run (it did not build or could
    /// not run) uses the code in `relative` (a path in the sandbox).
    fn skipped_may_reach(&self, _relative: &std::path::Path) -> bool {
        false
    }
}

/// How often a mutant goes back in the packs after a run hit its time limit
/// in another mutant's test, before it gets a solo run.
const CUT_SHORT: u32 = 3;

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
    let mut finished = 0;
    for language in [sandbox::Kind::CSharp, sandbox::Kind::Node] {
        let failures = result.failed.len();
        if run_one(language, targets, options, &mut cache, &mut result)
            && result.failed.len() == failures
        {
            finished += 1;
        }
    }
    note_filtered(targets, options, &mut result);
    timings(targets, options, &mut cache, &mut result);
    cache.save()?;
    answer(result, finished)
}

/// With no language finished, the failures are the whole answer: each of
/// them, not only the first.
fn answer(result: RunResult, finished: usize) -> Result<RunResult, String> {
    if finished == 0 && !result.failed.is_empty() {
        return Err(result.failed.join("\n"));
    }
    Ok(result)
}

/// One language's mutants: from the cache where it has them, the rest
/// tested, and what this run learned stored. False when the language has
/// no mutants or its tests could not be set up.
fn run_one(
    language: sandbox::Kind,
    targets: &[Target],
    options: &Options,
    cache: &mut cache::Cache,
    result: &mut RunResult,
) -> bool {
    let pending = pending(language, targets, options, result);
    if pending.is_empty() {
        return false;
    }
    let scope = match Scope::new(language, &pending, options) {
        Ok(scope) => scope,
        Err(err) => {
            failed(language, err, &pending, result);
            return false;
        }
    };
    if options.use_cache {
        from_cache(cache, &pending, &scope, options, result);
    }
    let killed_by = test_open(language, &scope, &pending, options, result);
    store(cache, &pending, &scope, &killed_by, result);
    note_left_out(&scope, &pending, result);
    true
}

/// The targets in `language`; for TypeScript and JavaScript, only those with a test runner.
fn pending<'a>(
    language: sandbox::Kind,
    targets: &'a [Target],
    options: &Options,
    result: &mut RunResult,
) -> Vec<&'a Target> {
    let pending: Vec<&Target> = targets
        .iter()
        .filter(|t| sandbox::Kind::of(&t.path) == Some(language))
        .collect();
    if language == sandbox::Kind::Node {
        return with_runner(pending, options, result);
    }
    pending
}

/// The outcomes the cache has for these mutants and tests.
fn from_cache(
    cache: &mut cache::Cache,
    pending: &[&Target],
    scope: &Scope,
    options: &Options,
    result: &mut RunResult,
) {
    let mut found = cache.lookup(pending, scope);
    // A survivor already tested again against these tests is not
    // tested a third time: renders of other formats stay cheap.
    let rechecked = if options.rerun_survivors {
        cache.rechecked(pending, scope)
    } else {
        HashSet::new()
    };
    let retest: Vec<usize> = found
        .iter()
        .filter(|(id, outcome)| {
            options.rerun_survivors && outcome.retested_on_rerun() && !rechecked.contains(id)
        })
        .map(|(id, _)| *id)
        .collect();
    found.retain(|id, _| !result.outcomes.contains_key(id) && !retest.contains(id));
    result.retested.extend(retest);
    let kept = found.keys().filter(|id| rechecked.contains(id)).count();
    if kept > 0 {
        result.notes.push(format!("--rerun-survivors: {kept} survivors (marked `cached`) were already tested again, and neither the test files of the chosen test projects nor the test filter changed since, so they come from the cache; tests added in another project or outside the filter count only once the filter or test projects include them (or with --no-cache)"));
    }
    result.cached.extend(found.keys().copied());
    let mut tests = cache.reached_tests(pending, scope);
    tests.retain(|id, _| found.contains_key(id));
    result.reached.extend(tests);
    result.outcomes.extend(found);
}

/// Tests the mutants without an outcome yet (none with `--cached`), and
/// says which test killed which.
fn test_open(
    language: sandbox::Kind,
    scope: &Scope,
    pending: &[&Target],
    options: &Options,
    result: &mut RunResult,
) -> cache::KilledBy {
    let open: Vec<usize> = pending
        .iter()
        .flat_map(|t| &t.mutants)
        .map(|m| m.id)
        .filter(|id| !result.outcomes.contains_key(id))
        .collect();
    if open.is_empty() {
        return cache::KilledBy::new();
    }
    if options.cached_only {
        result.notes.push(format!(
            "--cached: {} mutants have no cached result and were not tested; run without --cached to test them",
            open.len()
        ));
        result
            .untested
            .extend(open.iter().map(|id| (*id, Untested::NotCached)));
        return cache::KilledBy::new();
    }
    let total = pending.iter().map(|t| t.mutants.len()).sum::<usize>();
    progress(&format!(
        "{} of {total} mutants come from the cache; testing the other {} (new or changed code, changed tests, or never tested; --cached tests nothing)",
        total - open.len(),
        open.len()
    ));
    run_language(scope, pending, options, result).unwrap_or_else(|err| {
        failed(language, err, pending, result);
        cache::KilledBy::new()
    })
}

/// Stores what this run tested: cached outcomes keep the evidence they have.
fn store(
    cache: &mut cache::Cache,
    pending: &[&Target],
    scope: &Scope,
    killed_by: &cache::KilledBy,
    result: &RunResult,
) {
    let fresh: HashMap<usize, Outcome> = result
        .outcomes
        .iter()
        .filter(|(id, _)| !result.cached.contains(id))
        .map(|(id, outcome)| (*id, *outcome))
        .collect();
    cache.store(
        pending,
        scope,
        &fresh,
        killed_by,
        &result.reached_files,
        &result.reached,
    );
}

/// Records that `language`'s run failed: its mutants without a result stay
/// untested, and the other language's results are still reported.
fn failed(language: sandbox::Kind, err: String, pending: &[&Target], result: &mut RunResult) {
    for m in pending.iter().flat_map(|t| &t.mutants) {
        if !result.outcomes.contains_key(&m.id) {
            result.untested.insert(m.id, Untested::Failed);
        }
    }
    let name = match language {
        sandbox::Kind::CSharp => "C#",
        sandbox::Kind::Node => "TypeScript/JavaScript",
    };
    let failure = format!("the {name} run failed: {err}");
    result.notes.push(format!(
        "{failure}; its mutants are listed as not tested, and the results above are the other language's"
    ));
    result.failed.push(failure);
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
            options.jobs,
        )?),
        sandbox::Kind::Node => Box::new(node::NodeTests::prepare(
            &sandbox.dir,
            &files,
            options.jobs,
        )?),
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
    log_start(&prepared, scope, options, started);
    let built = Instant::now();
    progress(&format!(
        "building the sandbox (preparing it took {:.1} s)",
        secs(started)
    ));
    // A test project that does not build in a sandbox with only the tested
    // projects may build in a full one: it is only left out of a full one.
    let full = scope.roots.as_slice() == std::slice::from_ref(&scope.root);
    let mut status = build::build(
        &mut prepared,
        scope.kind,
        &mut result.outcomes,
        budget,
        full,
    );
    if status.is_err() && !full {
        result.notes.push("the sandbox with only the tested projects did not build, so the whole repository was copied; set `sandbox = \"full\"` in [mutate] to skip the first attempt".into());
        drop(prepared);
        prepared = prepare(
            scope,
            std::slice::from_ref(&scope.root),
            targets,
            options,
            &result.outcomes,
        )?;
        status = build::build(
            &mut prepared,
            scope.kind,
            &mut result.outcomes,
            budget,
            true,
        );
    }
    result.build += built.elapsed();
    debug!("build", "{:.1} s, {}", secs(built), build_text(&status));
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
    let unchanged = settle_unchanged(&mut reach, result);
    let tested = Instant::now();
    let candidates: Vec<usize> = prepared
        .files
        .iter()
        .flat_map(|f| f.mutants.iter().map(|m| m.id))
        .filter(|id| !unchanged.contains(id))
        .collect();
    let killers = test_mutants(
        prepared.engine.as_ref(),
        &candidates,
        &reach,
        options,
        result,
    )?;
    result.mutants += tested.elapsed();
    log_end(&candidates, result, tested);
    untested_by_skipped(&prepared, &candidates, result);
    result.notes.extend(prepared.engine.notes());
    mark_untested(&prepared.files, result, Untested::TestRuns);
    Ok(killer_files(killers, &reach))
}

/// A mutant no test that ran reaches, in code a skipped test project uses,
/// is not tested rather than without tests: that project's tests may reach
/// it. Not stored either, so the next run, with the project fixed, tests it.
fn untested_by_skipped(prepared: &Prepared, candidates: &[usize], result: &mut RunResult) {
    let candidates: HashSet<usize> = candidates.iter().copied().collect();
    let unreached: Vec<usize> = prepared
        .files
        .iter()
        .filter(|file| prepared.engine.skipped_may_reach(&file.relative))
        .flat_map(|file| file.mutants.iter().map(|m| m.id))
        .filter(|id| {
            candidates.contains(id) && result.outcomes.get(id) == Some(&Outcome::NoCoverage)
        })
        .collect();
    for id in unreached {
        result.outcomes.remove(&id);
        result.untested.insert(id, Untested::SkippedProject);
    }
}

fn log_start(prepared: &Prepared, scope: &Scope, options: &Options, started: Instant) {
    log::open(&exec::runs_folder(&prepared.sandbox.dir).join("dgf-mutate.log"));
    debug!(
        "run",
        "dgf {} {:?}: {} mutants in {} files, jobs {}{}, sandbox {}, prepared in {:.1} s",
        env!("CARGO_PKG_VERSION"),
        scope.kind,
        prepared
            .files
            .iter()
            .map(|f| f.mutants.len())
            .sum::<usize>(),
        prepared.files.len(),
        options.jobs,
        if options.low_priority {
            " at low priority"
        } else {
            ""
        },
        prepared.sandbox.dir.display(),
        secs(started)
    );
}

fn build_text(status: &Result<build::Built, String>) -> String {
    match status {
        Ok(build::Built::OutOfTime) => "out of time".to_string(),
        Ok(_) => "built".to_string(),
        Err(err) => format!("failed: {err}"),
    }
}

/// The mutants whose value no test changed: they survive without a run.
fn settle_unchanged(reach: &mut Reach, result: &mut RunResult) -> BTreeSet<usize> {
    let reaching: HashMap<usize, Vec<String>> = reach
        .probed
        .iter()
        .map(|id| (*id, reaching_tests(reach, *id)))
        .collect();
    let unchanged = reach.unchanged();
    debug!(
        "reach",
        "{:.1} s; {} tests ({} failing without mutants), {} mutants reached by tests, {} outside any test, {} checked for a change of value, {} never changed (survive without a run)",
        reach.duration.as_secs_f64(),
        reach.times.len(),
        reach.failing.len(),
        reach.by_mutant.len(),
        reach.outside.len(),
        reach.probed.len(),
        unchanged.len()
    );
    for id in &unchanged {
        result.outcomes.insert(*id, Outcome::Survived);
        result.unchanged.insert(*id);
        if let Some(tests) = reaching.get(id) {
            result.reached.insert(*id, tests.clone());
        }
    }
    unchanged
}

fn log_end(candidates: &[usize], result: &RunResult, tested: Instant) {
    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    for outcome in candidates.iter().filter_map(|id| result.outcomes.get(id)) {
        *tally.entry(format!("{outcome:?}")).or_default() += 1;
    }
    debug!(
        "mutants",
        "{:.1} s, {} test runs ({} second runs after a timeout): {tally:?}",
        secs(tested),
        result.test_runs,
        result.retries
    );
    if let Some(peak) = log::peak_memory() {
        debug!(
            "memory",
            "dgf itself: peak working set {:.0} MB",
            peak as f64 / 1048576.0
        );
    }
    if let Some(path) = log::path() {
        progress(&format!("debug log: {}", path.display()));
    }
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
    if reach.failing.is_empty() && reach.flaky.is_empty() {
        return;
    }
    let failing = one_spelling(&reach.failing);
    if !failing.is_empty() {
        let names = &failing[..failing.len().min(5)];
        let note = format!(
            "{} tests fail without any mutant and are left out, so mutants only they reach look untested: {names:?}",
            failing.len()
        );
        tellln!("note: {note}");
        result.notes.push(note);
    }
    result.set_aside.extend(reach.failing.iter().cloned());
    let flaky = one_spelling(&reach.flaky);
    if !flaky.is_empty() {
        let names = &flaky[..flaky.len().min(5)];
        let note = format!(
            "{} tests failed without any mutant and passed when run again, so they may depend on other tests or on timing: {names:?}; a kill by one of them is less certain",
            flaky.len()
        );
        tellln!("note: {note}");
        result.notes.push(note);
    }
}

/// `tests` with a name stored in both frameworks' spellings ("a > b" and
/// "a b") given once.
fn one_spelling(tests: &BTreeSet<String>) -> Vec<&String> {
    tests
        .iter()
        .filter(|test| {
            test.contains(" > ")
                || !tests
                    .iter()
                    .any(|other| other.contains(" > ") && other.replace(" > ", " ") == **test)
        })
        .collect()
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
pub(super) fn progress(step: &str) {
    tellln!("  {step}...");
}

fn secs(since: Instant) -> f64 {
    since.elapsed().as_secs_f64()
}

pub use dog_food_core::fnv;

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
    let Packed { solo, fresh, tried } = if options.no_mix {
        Packed {
            solo: reached.clone(),
            fresh: BTreeSet::new(),
            tried: HashMap::new(),
        }
    } else {
        packed(engine, &reached, reach, options, result, &mut killers)?
    };
    let runner = Runner::new(engine, reach, options)?;
    // Solo runs in warm processes first; what they cannot be trusted on
    // (a kill the check pinned on the process, a mutant that never took
    // effect) runs once more in a fresh process, with the mutant on from
    // the start, which is final.
    let mut again = Vec::new();
    for (id, run, tag) in runner.solos(&solo, &fresh) {
        if let Some(id) = solo_done(engine, id, run, &tag, reach, result, &mut killers) {
            again.push(id);
        }
    }
    let all: BTreeSet<usize> = again.iter().copied().collect();
    for (id, run, tag) in runner.solos(&again, &all) {
        solo_done(engine, id, run, &tag, reach, result, &mut killers);
    }
    let (started, retries) = runner.finish();
    result.test_runs += started;
    result.retries += retries;
    note_partial(&reached, reach, &tried, result);
    Ok(killers)
}

/// Records what came of a solo run; returns the mutant when the run cannot
/// be trusted and a fresh process is needed. A run that could not start is
/// not the mutant's doing, and not the budget's: the mutant is left
/// untested, with the error.
fn solo_done(
    engine: &dyn Engine,
    id: usize,
    run: Result<Ran<SoloRun>, String>,
    tag: &str,
    reach: &Reach,
    result: &mut RunResult,
    killers: &mut HashMap<usize, Vec<String>>,
) -> Option<usize> {
    match run {
        Ok(Ran::Done(run)) => match solo_result(engine, id, &run, tag, reach, result) {
            Some(killed_by) => {
                killers.insert(id, killed_by);
            }
            None => return Some(id),
        },
        Ok(Ran::Skipped) => {}
        Err(err) => {
            debug!("solo", "{tag}: could not run: {err}");
            result.untested.insert(id, Untested::RunError);
            result.run_errors.insert(id, err);
        }
    }
    None
}

/// The evidence a reached mutant has when it was left undecided: the tests
/// that ran and passed, of those that reach it.
fn note_partial(
    reached: &[usize],
    reach: &Reach,
    tried: &HashMap<usize, BTreeSet<String>>,
    result: &mut RunResult,
) {
    for id in reached {
        if result.outcomes.contains_key(id) || result.run_errors.contains_key(id) {
            continue;
        }
        if let Some(tests) = reach.by_mutant.get(id) {
            let ran = tried.get(id).map_or(0, BTreeSet::len);
            result.partial.insert(*id, (ran, tests.len()));
        }
    }
}

/// What the packs left: the mutants for solo runs (each once), those of
/// them that need a fresh process, and per mutant the tests that ran and
/// passed.
struct Packed {
    solo: Vec<usize>,
    fresh: BTreeSet<usize>,
    tried: HashMap<usize, BTreeSet<String>>,
}

/// What a pack found for each of its mutants, besides the ones it killed.
#[derive(Debug, Default, PartialEq, Eq)]
struct Resolved {
    /// Every planned test passed and the mutant took effect in each. Not
    /// survivors yet: a mutant may have untried tests, and is a survivor
    /// only when none is left.
    passed: Vec<usize>,
    /// A planned test did not report, the run timed out, or the run could
    /// not start: for a solo run.
    undecided: Vec<usize>,
    /// The run cannot be trusted on these: their only kills are polluted,
    /// or their mutant never took effect in a planned test. For a solo run
    /// in a fresh process.
    fresh: Vec<usize>,
    /// The run hit its time limit in another mutant's test before these
    /// mutants' tests all ran, with the ones that did and passed: they go
    /// back in the next round's packs.
    cut_short: Vec<(usize, BTreeSet<String>)>,
}

/// Runs the reached mutants in packs, in rounds: most fail their quickest
/// test, so each gets one test first, and each round's survivors their next
/// untried tests, twice as many each round, so a killed mutant's other tests
/// never run and the slowest tests only run for what the quick ones miss.
/// A mutant is a survivor only once every test that reaches it has passed;
/// one the budget or a failed run cut short keeps no outcome. Returns the
/// mutants the packs could not decide, for solo runs, each once.
fn packed(
    engine: &dyn Engine,
    reached: &[usize],
    reach: &Reach,
    options: &Options,
    result: &mut RunResult,
    killers: &mut HashMap<usize, Vec<String>>,
) -> Result<Packed, String> {
    let mut solo = BTreeSet::new();
    let mut fresh = BTreeSet::new();
    let mut tried: HashMap<usize, BTreeSet<String>> = HashMap::new();
    let mut cut: HashMap<usize, u32> = HashMap::new();
    let mut open = reached.to_vec();
    let mut count = 1;
    progress(&format!(
        "{} mutants are reached by tests: first one test each, {} at a time",
        reached.len(),
        options.jobs
    ));
    while !open.is_empty() {
        let (next, round, done) = next_round(&open, reach, &tried, count);
        // No untried test left, and every tried one passed: a survivor.
        settle_survivors(engine, done, &mut solo, result);
        if round.is_empty() {
            break;
        }
        if count > 1 {
            progress(&format!(
                "{} survivors so far: up to {count} more tests each",
                round.len()
            ));
        }
        let resolved = run_packs(engine, &round, &next, options, result, killers)?;
        remember_tried(&mut tried, &resolved.passed, &next);
        solo.extend(resolved.undecided);
        solo.extend(resolved.fresh.iter().copied());
        fresh.extend(resolved.fresh);
        open = resolved.passed;
        for (id, ran) in resolved.cut_short {
            let times = cut.entry(id).or_insert(0);
            *times += 1;
            take_cut_short(id, ran, *times, &mut tried, &mut open, &mut solo);
        }
        count *= 2;
    }
    Ok(Packed {
        solo: solo.into_iter().collect(),
        fresh,
        tried,
    })
}

/// Mutants with no untried test left, all of whose tried tests passed: survivors,
/// or for a runner that wants it, run alone first.
fn settle_survivors(
    engine: &dyn Engine,
    done: Vec<usize>,
    solo: &mut BTreeSet<usize>,
    result: &mut RunResult,
) {
    for id in done {
        if engine.confirms_survivors() {
            solo.insert(id);
        } else {
            result.outcomes.insert(id, Outcome::Survived);
        }
    }
}

/// A mutant whose pack timed out goes on in the next round with the tests that
/// did run counted. Each timed-out pack gave up on the mutant whose test was
/// running, so this ends; the limit is for a run that keeps stopping anyway.
fn take_cut_short(
    id: usize,
    ran: BTreeSet<String>,
    times: u32,
    tried: &mut HashMap<usize, BTreeSet<String>>,
    open: &mut Vec<usize>,
    solo: &mut BTreeSet<usize>,
) {
    if times > CUT_SHORT {
        solo.insert(id);
    } else {
        tried.entry(id).or_default().extend(ran);
        open.push(id);
    }
}

/// The next round's tests for the `open` mutants (`count` each, plus the
/// mutants reached outside any test in the first round), the mutants that
/// got some, and the ones with no untried test left.
fn next_round(
    open: &[usize],
    reach: &Reach,
    tried: &HashMap<usize, BTreeSet<String>>,
    count: usize,
) -> (Reach, Vec<usize>, Vec<usize>) {
    let next = Reach {
        outside: if count == 1 {
            reach.outside.clone()
        } else {
            HashSet::new()
        },
        ..next_tests(open, reach, tried, count)
    };
    let (round, done) = open
        .iter()
        .copied()
        .partition(|id| next.by_mutant.contains_key(id) || next.outside.contains(id));
    (next, round, done)
}

/// The tests the round's `passed` mutants have now passed.
fn remember_tried(tried: &mut HashMap<usize, BTreeSet<String>>, passed: &[usize], next: &Reach) {
    for id in passed {
        if let Some(tests) = next.by_mutant.get(id) {
            tried.entry(*id).or_default().extend(tests.iter().cloned());
        }
    }
}

/// Records a solo run's outcome, a crash that killed it, and why a run
/// crashed; returns the tests that killed it, or `None` when the run cannot
/// be trusted and the mutant needs a fresh process.
fn solo_result(
    engine: &dyn Engine,
    id: usize,
    run: &SoloRun,
    tag: &str,
    reach: &Reach,
    result: &mut RunResult,
) -> Option<Vec<String>> {
    let outside = reach.outside.contains(&id);
    let planned = (!outside)
        .then(|| reach.by_mutant.get(&id))
        .flatten()
        .map(|tests| (tests, id));
    let verdict = solo_outcome(run, &reach.failing, planned);
    debug!(
        "solo",
        "{tag}: mutant {id}{}, {} tests reported, run {:?}: {verdict:?}",
        if outside { " (outside any test)" } else { "" },
        run.1.outcome.len(),
        run.0
    );
    let Verdict::Decided(outcome, killed_by) = verdict else {
        return None;
    };
    if outcome == Outcome::Killed && crashed(run.0, &run.1) {
        result.crash_kills.insert(id);
    }
    if let Some(reason) = (outcome == Outcome::RuntimeError)
        .then(|| engine.crash_reason(tag))
        .flatten()
    {
        result.crash_reasons.insert(id, reason);
    }
    result.outcomes.insert(id, outcome);
    Some(killed_by)
}

/// Records which tests reach each mutant, then packs them (or, with
/// `--no-mix`, runs each alone).
fn record_reach(reached: &[usize], reach: &Reach, result: &mut RunResult) {
    for id in reached {
        result.reached.insert(*id, reaching_tests(reach, *id));
        result.reached_files.insert(*id, reaching_files(reach, *id));
    }
}

/// Packs `mutants` with the tests `reach` gives them and runs the packs.
/// Mutants reached outside any test, and those of a pack that could not
/// start, are undecided; a pack the budget skipped leaves its mutants with
/// nothing.
fn run_packs(
    engine: &dyn Engine,
    mutants: &[usize],
    reach: &Reach,
    options: &Options,
    result: &mut RunResult,
    killers: &mut HashMap<usize, Vec<String>>,
) -> Result<Resolved, String> {
    let (packs, outside) = pack(mutants, reach, PACK_TESTS);
    debug!(
        "packs",
        "{} mutants in {} packs, {} to run alone (reached outside any test)",
        mutants.len() - outside.len(),
        packs.len(),
        outside.len()
    );
    let mut resolved = Resolved {
        undecided: outside,
        ..Resolved::default()
    };
    let runner = Runner::new(engine, reach, options)?;
    for (index, (pack, run)) in packs.iter().zip(runner.packs(&packs)).enumerate() {
        match run {
            Ok(Ran::Done(run)) => {
                let found = resolve(pack, reach, &run, &mut result.outcomes, killers);
                debug!(
                    "packs",
                    "p{index}: {} tests, {} mutants: {} killed, {} undecided, {} for a fresh process{}",
                    pack.plan.len(),
                    pack.mutants.len(),
                    pack.mutants.len()
                        - found.passed.len()
                        - found.undecided.len()
                        - found.fresh.len(),
                    found.undecided.len(),
                    found.fresh.len(),
                    if run.as_ref().is_none_or(|r| r.timed_out) {
                        " (timed out)"
                    } else {
                        ""
                    }
                );
                resolved.passed.extend(found.passed);
                resolved.undecided.extend(found.undecided);
                resolved.fresh.extend(found.fresh);
                resolved.cut_short.extend(found.cut_short);
            }
            Ok(Ran::Skipped) => debug!("packs", "p{index}: skipped, no time left"),
            Err(err) => {
                debug!("packs", "p{index}: could not run: {err}");
                resolved.undecided.extend(pack.mutants.iter().copied());
            }
        }
    }
    let (started, again) = runner.finish();
    result.test_runs += started;
    result.retries += again;
    Ok(resolved)
}

/// What a test that needs a pack to itself costs on top of its own time.
const NEEDS_A_PACK: Duration = Duration::from_secs(2);

/// Up to `count` tests per mutant that it has not `tried`: the cheapest,
/// where a test costs its time plus half a second for every mutant it
/// already carries (each one needs a pack of its own).
fn next_tests(
    mutants: &[usize],
    reach: &Reach,
    tried: &HashMap<usize, BTreeSet<String>>,
    count: usize,
) -> Reach {
    let none = BTreeSet::new();
    let untried = |id: &usize| -> Vec<&String> {
        let tried = tried.get(id).unwrap_or(&none);
        reach.by_mutant.get(id).map_or(Vec::new(), |tests| {
            tests.iter().filter(|t| !tried.contains(*t)).collect()
        })
    };
    let mut order: Vec<(usize, Vec<&String>)> = mutants
        .iter()
        .map(|id| (*id, untried(id)))
        .filter(|(_, tests)| !tests.is_empty())
        .collect();
    order.sort_by_key(|(id, tests)| (tests.len(), *id));
    let mut load: HashMap<&String, u32> = HashMap::new();
    let mut by_mutant = HashMap::new();
    for (id, mut tests) in order {
        tests.sort_by_key(|t| {
            let time = reach.times.get(*t).copied().unwrap_or_default();
            let alone = if reach.queuing.contains(*t) {
                NEEDS_A_PACK
            } else {
                Duration::ZERO
            };
            time + alone + Duration::from_millis(500) * load.get(t).copied().unwrap_or(0)
        });
        tests.truncate(count);
        for test in &tests {
            *load.entry(*test).or_default() += 1;
        }
        by_mutant.insert(id, tests.into_iter().cloned().collect::<BTreeSet<_>>());
    }
    Reach {
        by_mutant,
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
                        |run| run.as_ref().is_none_or(|r| r.timed_out),
                    )
                })
                .collect()
        })
    }

    /// Each mutant's solo run and the tag of its last run; the mutants in
    /// `fresh` run in a new process with the mutant on from the start.
    fn solos(
        &self,
        ids: &[usize],
        fresh: &BTreeSet<usize>,
    ) -> Vec<(usize, Result<Ran<SoloRun>, String>, String)> {
        self.pool.install(|| {
            ids.par_iter()
                .map(|&id| {
                    let tests = (!self.reach.outside.contains(&id))
                        .then(|| self.reach.by_mutant.get(&id))
                        .flatten();
                    let limit = self.timeouts.limit(self.reach, tests);
                    let fresh = fresh.contains(&id);
                    let solo = |limit: Duration, tag: &str| {
                        self.within_budget(
                            limit,
                            |limit| self.engine.solo(id, self.reach, fresh, limit, tag),
                            |run| run.0 == TestRun::TimedOut,
                        )
                    };
                    let mut tag = format!("s{id}{}", if fresh { "-fresh" } else { "" });
                    let mut run = solo(limit, &tag);
                    // A busy machine makes slow runs, not endless ones: a
                    // timeout counts only when a run with four times the time
                    // (and at least 30 s more) also times out. (A run's
                    // processor time cannot tell the two apart: parallel
                    // tests, JIT and GC use processor time on many threads.)
                    if matches!(&run, Ok(Ran::Done((TestRun::TimedOut, _)))) {
                        let longer = (limit * 4).max(limit + Duration::from_secs(30));
                        self.again
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tag.push_str("-again");
                        run = solo(longer, &tag);
                    }
                    (id, run, tag)
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
            p.used.len() + tests.len() <= max_tests
                && tests.iter().all(|t| !p.used.contains(t))
                && keeps_leaks_out(p, tests, reach)
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

/// Whether `tests` can join `pack` without a test that can leave state behind
/// running next to another mutated test of its home.
fn keeps_leaks_out(pack: &Pack, tests: &BTreeSet<String>, reach: &Reach) -> bool {
    if reach.queuing.is_empty() {
        return true;
    }
    tests.iter().all(|test| {
        let home = reach.home.get(test);
        let mut neighbours = pack.used.iter().filter(|u| reach.home.get(*u) == home);
        if reach.queuing.contains(test) {
            neighbours.next().is_none()
        } else {
            !neighbours.any(|u| reach.queuing.contains(u))
        }
    })
}

fn place(pack: &mut Pack, id: usize, tests: &BTreeSet<String>) {
    pack.mutants.push(id);
    for test in tests {
        pack.used.insert(test.clone());
        pack.plan.push((test.clone(), id));
    }
}

/// Records the mutants a pack killed, with their killing tests; the rest
/// passed their planned tests, could not be decided (a test did not
/// report, or the run timed out), or need a fresh process (their only
/// kills are polluted, or their mutant never took effect in a planned
/// test).
fn resolve(
    pack: &Pack,
    reach: &Reach,
    run: &PackRun,
    outcomes: &mut HashMap<usize, Outcome>,
    killers: &mut HashMap<usize, Vec<String>>,
) -> Resolved {
    let Some(results) = run else {
        return Resolved {
            undecided: pack.mutants.clone(),
            ..Resolved::default()
        };
    };
    let mut found = Resolved::default();
    for &id in &pack.mutants {
        match pack_verdict(&reach.by_mutant[&id], id, results) {
            PackVerdict::Killed(real) => {
                outcomes.insert(id, Outcome::Killed);
                killers.insert(id, real);
            }
            PackVerdict::Fresh => found.fresh.push(id),
            PackVerdict::CutShort(ran) => found.cut_short.push((id, ran)),
            PackVerdict::Undecided => found.undecided.push(id),
            PackVerdict::Passed => found.passed.push(id),
        }
    }
    found
}

/// What a pack's run says about one of its mutants.
enum PackVerdict {
    /// Killed, by these tests.
    Killed(Vec<String>),
    /// Only a fresh process can tell.
    Fresh,
    /// The run stopped before these tests' mutant was done: they did run.
    CutShort(BTreeSet<String>),
    Undecided,
    Passed,
}

fn pack_verdict(tests: &BTreeSet<String>, id: usize, results: &TestResults) -> PackVerdict {
    // A failing test kills the mutant whatever the others do, so a run
    // may skip a killed mutant's other tests.
    let failed: Vec<&String> = tests
        .iter()
        .filter(|t| results.outcome.get(*t) == Some(&false))
        .collect();
    let real: Vec<String> = failed
        .iter()
        .filter(|t| !results.polluted.contains(**t))
        .map(|t| t.to_string())
        .collect();
    if !real.is_empty() {
        return PackVerdict::Killed(real);
    }
    if !failed.is_empty() {
        return PackVerdict::Fresh;
    }
    if tests.iter().any(|t| !results.outcome.contains_key(t)) {
        return unreported(tests, id, results);
    }
    if tests.iter().any(|t| !results.activated(t, id)) {
        return PackVerdict::Fresh;
    }
    PackVerdict::Passed
}

/// A mutant some of whose tests never reported.
fn unreported(tests: &BTreeSet<String>, id: usize, results: &TestResults) -> PackVerdict {
    if !results.timed_out || tests.iter().any(|t| results.running.contains(t)) {
        return PackVerdict::Undecided;
    }
    PackVerdict::CutShort(
        tests
            .iter()
            .filter(|t| results.outcome.contains_key(*t) && results.activated(t, id))
            .cloned()
            .collect(),
    )
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

    /// A pack stopped at its time limit: the mutant whose test was running
    /// gets a solo run, a mutant whose tests had not started goes back in
    /// the packs with what it already passed, and what was decided stands.
    #[test]
    fn a_test_stored_under_both_spellings_counts_once() {
        let tests: BTreeSet<String> = ["a > b", "a b", "plain", "c > d", "c d"]
            .map(String::from)
            .into();
        assert_eq!(one_spelling(&tests), ["a > b", "c > d", "plain"]);
    }

    #[test]
    fn a_timed_out_pack_keeps_what_it_reported() {
        let reach = reach(&[
            (1, &["Passed", "Later"]),
            (2, &["Hangs"]),
            (3, &["Failed"]),
            (4, &["Whole"]),
        ]);
        let mut pack = Pack::default();
        for id in [1, 2, 3, 4] {
            place(&mut pack, id, &reach.by_mutant[&id]);
        }
        let outcome = [("Passed", true), ("Failed", false), ("Whole", true)]
            .map(|(test, passed)| (test.to_string(), passed));
        let run = Some(TestResults {
            timed_out: true,
            running: set(&["Hangs"]),
            ..TestResults::of(outcome.into())
        });
        let mut outcomes = HashMap::new();
        let mut killers = HashMap::new();
        let found = resolve(&pack, &reach, &run, &mut outcomes, &mut killers);
        assert_eq!(found.undecided, [2]);
        assert_eq!(found.passed, [4]);
        assert_eq!(found.cut_short, [(1, set(&["Passed"]))]);
        assert_eq!(outcomes[&3], Outcome::Killed);
        assert!(found.fresh.is_empty());
    }

    /// With nothing said about what was running, nothing can be kept apart.
    #[test]
    fn a_timed_out_pack_that_names_no_running_test_decides_only_what_finished() {
        let reach = reach(&[(1, &["A"]), (2, &["B"])]);
        let mut pack = Pack::default();
        for id in [1, 2] {
            place(&mut pack, id, &reach.by_mutant[&id]);
        }
        let mut outcomes = HashMap::new();
        let found = resolve(&pack, &reach, &None, &mut outcomes, &mut HashMap::new());
        assert_eq!(found.undecided, [1, 2]);
        assert!(found.cut_short.is_empty());
    }

    #[test]
    fn tests_failing_without_mutants_decide_nothing() {
        let failing = set(&["Broken"]);
        let results = |broken: bool, other: bool| -> SoloRun {
            let tests = [("Broken".to_string(), broken), ("Other".to_string(), other)];
            (TestRun::Failed, TestResults::of(tests.into()))
        };
        assert_eq!(
            solo_outcome(&results(false, true), &failing, None),
            Verdict::Decided(Outcome::Survived, vec![])
        );
        assert_eq!(
            solo_outcome(&results(false, false), &failing, None),
            Verdict::Decided(Outcome::Killed, vec!["Other".to_string()])
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
        let outcome = |run: &SoloRun| match solo_outcome(run, &none, None) {
            Verdict::Decided(outcome, _) => outcome,
            Verdict::Fresh => panic!("decided"),
        };
        let one = |test: &str, passed: bool| TestResults::of([(test.to_string(), passed)].into());
        assert_eq!(
            outcome(&(TestRun::Failed, TestResults::default())),
            Outcome::RuntimeError
        );
        // The host said it crashed, with one test reported first.
        let partial: SoloRun = (TestRun::Crashed, one("A", true));
        assert_eq!(outcome(&partial), Outcome::RuntimeError);
        // A failing test counts even in a crashed run; the kill is flagged
        // by `solo_result`, from the same status.
        let killed: SoloRun = (TestRun::Crashed, one("A", false));
        assert_eq!(outcome(&killed), Outcome::Killed);
        assert!(crashed(killed.0, &killed.1));
        // A failed run with a test missing is not a crash by itself: a worker
        // skips a killed mutant's other tests.
        let skipped: SoloRun = (TestRun::Failed, one("A", true));
        assert!(!crashed(skipped.0, &skipped.1));
        assert_eq!(outcome(&skipped), Outcome::Survived);
        let timed_out: SoloRun = (TestRun::TimedOut, TestResults::default());
        assert_eq!(outcome(&timed_out), Outcome::Timeout);
        assert!(!Outcome::RuntimeError.counts() && Outcome::NoCoverage.counts());
    }

    #[test]
    fn a_polluted_kill_or_an_inert_mutant_needs_a_fresh_process() {
        let none = BTreeSet::new();
        let planned = set(&["A", "B"]);
        // The kill check pinned the only failure on the process.
        let mut polluted =
            TestResults::of([("A".to_string(), false), ("B".to_string(), true)].into());
        polluted.polluted.insert("A".into());
        assert_eq!(
            solo_outcome(
                &(TestRun::Failed, polluted.clone()),
                &none,
                Some((&planned, 7))
            ),
            Verdict::Fresh
        );
        // Another failure that is the mutant's own still kills it.
        polluted.outcome.insert("B".into(), false);
        assert_eq!(
            solo_outcome(&(TestRun::Failed, polluted), &none, Some((&planned, 7))),
            Verdict::Decided(Outcome::Killed, vec!["B".into()])
        );
        // Every test passed, but the mutant never took effect in B.
        let mut inert = TestResults::of([("A".to_string(), true), ("B".to_string(), true)].into());
        inert.activated = Some([("A".to_string(), 7)].into());
        assert_eq!(
            solo_outcome(
                &(TestRun::Passed, inert.clone()),
                &none,
                Some((&planned, 7))
            ),
            Verdict::Fresh
        );
        // It took effect in both: a survivor. A run that cannot tell is trusted.
        inert.activated = Some([("A".to_string(), 7), ("B".to_string(), 7)].into());
        assert_eq!(
            solo_outcome(
                &(TestRun::Passed, inert.clone()),
                &none,
                Some((&planned, 7))
            ),
            Verdict::Decided(Outcome::Survived, vec![])
        );
        inert.activated = None;
        assert_eq!(
            solo_outcome(&(TestRun::Passed, inert), &none, Some((&planned, 7))),
            Verdict::Decided(Outcome::Survived, vec![])
        );
    }

    #[test]
    fn a_pack_sends_polluted_kills_and_inert_mutants_to_a_fresh_process() {
        let reach = reach(&[(0, &["A"]), (1, &["B"]), (2, &["C"]), (3, &["D"])]);
        let (packs, _) = pack(&[0, 1, 2, 3], &reach, 100);
        let mut results = TestResults::of(
            [("A", false), ("B", false), ("C", true), ("D", true)]
                .iter()
                .map(|(t, p)| (t.to_string(), *p))
                .collect(),
        );
        results.polluted.insert("B".into());
        results.activated = Some([("A".to_string(), 0), ("C".to_string(), 2)].into());
        let mut outcomes = HashMap::new();
        let mut killers = HashMap::new();
        let found = resolve(
            &packs[0],
            &reach,
            &Some(results),
            &mut outcomes,
            &mut killers,
        );
        assert_eq!(outcomes[&0], Outcome::Killed);
        assert!(!outcomes.contains_key(&1), "polluted: not a kill");
        assert_eq!(
            found,
            Resolved {
                passed: vec![2],
                undecided: vec![],
                fresh: vec![1, 3],
                cut_short: vec![],
            }
        );
    }

    #[test]
    fn a_solo_run_that_cannot_be_trusted_runs_once_more_in_a_fresh_process() {
        let fake = Fake {
            pack: Box::new(|_| Ok(None)),
            solo: Box::new(|mutant| {
                let mut results = passed(&["A"]);
                if mutant == 1 {
                    // Warm: A fails, but fails without the mutant too.
                    results.outcome.insert("A".into(), false);
                    results.polluted.insert("A".into());
                }
                Ok((TestRun::Failed, results))
            }),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let reach = timed_reach(&[(1, &["A"]), (2, &["A"])]);
        let mut result = RunResult::default();
        test_mutants(
            &fake,
            &[1, 2],
            &reach,
            &options(std::path::Path::new(".")),
            &mut result,
        )
        .unwrap();
        // The fake's fresh run reports the same polluted failure, which a
        // fresh process never does: there it is the mutant's, so a kill.
        assert_eq!(result.outcomes[&2], Outcome::Survived);
        let calls = fake.calls();
        assert!(calls.contains(&"s1 mutant 1".to_string()), "{calls:?}");
        assert!(
            calls.contains(&"s1-fresh mutant 1 fresh".to_string()),
            "{calls:?}"
        );
        assert!(
            !calls.contains(&"s2-fresh mutant 2 fresh".to_string()),
            "{calls:?}"
        );
    }

    type PackScript = Box<dyn Fn(&Pack) -> Result<PackRun, String> + Send + Sync>;
    type SoloScript = Box<dyn Fn(usize) -> Result<SoloRun, String> + Send + Sync>;

    /// An engine whose packs and solo runs are scripted.
    struct Fake {
        pack: PackScript,
        solo: SoloScript,
        calls: std::sync::Mutex<Vec<String>>,
    }

    impl Fake {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Engine for Fake {
        fn build(&self, _: Round, _: Budget) -> Result<(), BuildError> {
            Ok(())
        }
        fn locate(&self, _: &[FileMutants], _: &[String]) -> exec::Located {
            exec::Located::default()
        }
        fn reach(&self, _: Budget) -> Result<Option<Reach>, String> {
            Ok(None)
        }
        fn pack(&self, pack: &Pack, _: &Reach, _: Duration, tag: &str) -> Result<PackRun, String> {
            let tests: Vec<&str> = pack.plan.iter().map(|(t, _)| t.as_str()).collect();
            self.calls
                .lock()
                .unwrap()
                .push(format!("{tag} {}", tests.join(",")));
            (self.pack)(pack)
        }
        fn solo(
            &self,
            mutant: usize,
            _: &Reach,
            fresh: bool,
            _: Duration,
            tag: &str,
        ) -> Result<SoloRun, String> {
            self.calls.lock().unwrap().push(format!(
                "{tag} mutant {mutant}{}",
                if fresh { " fresh" } else { "" }
            ));
            (self.solo)(mutant)
        }
        fn test_projects(&self) -> Vec<String> {
            Vec::new()
        }
    }

    fn passed(tests: &[&str]) -> TestResults {
        TestResults::of(tests.iter().map(|t| (t.to_string(), true)).collect())
    }

    /// A reach where test `A` is the quickest, then `B`, then `C`.
    fn timed_reach(entries: &[(usize, &[&str])]) -> Reach {
        let mut reach = reach(entries);
        for (i, test) in ["A", "B", "C", "D"].iter().enumerate() {
            reach
                .times
                .insert(test.to_string(), Duration::from_millis(10 * (i as u64 + 1)));
        }
        reach
    }

    #[test]
    fn a_mutant_a_pack_could_not_decide_runs_solo_once_and_is_not_a_survivor() {
        // Round 1 (test A) passes; round 2 (B and C) times out.
        let fake = Fake {
            pack: Box::new(|pack| {
                let only_a = pack.plan.iter().all(|(t, _)| t == "A");
                Ok(only_a.then(|| passed(&["A"])))
            }),
            solo: Box::new(|_| Ok((TestRun::Passed, passed(&["A", "B", "C"])))),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let reach = timed_reach(&[(4, &["A", "B", "C"])]);
        let options = options(std::path::Path::new("."));
        let mut result = RunResult::default();
        let mut killers = HashMap::new();
        let packed = packed(&fake, &[4], &reach, &options, &mut result, &mut killers).unwrap();
        // Once in the solo list, and no round after the one that timed out.
        assert_eq!(packed.solo, [4]);
        assert_eq!(fake.calls(), ["p0 A", "p0 B,C"]);
        // Its first test passed, but it is not a survivor until every test has.
        assert!(!result.outcomes.contains_key(&4));
        assert_eq!(packed.tried[&4], set(&["A"]));
        // The solo run decides it.
        let killers = test_mutants(&fake, &[4], &reach, &options, &mut result).unwrap();
        assert_eq!(result.outcomes[&4], Outcome::Survived);
        assert!(killers[&4].is_empty());
        assert_eq!(fake.calls().last().unwrap(), "s4 mutant 4");
        assert!(result.partial.is_empty());
    }

    #[test]
    fn a_mutant_survives_only_once_every_test_that_reaches_it_passed() {
        let fake = Fake {
            pack: Box::new(|pack| {
                Ok(Some(TestResults::of(
                    pack.plan
                        .iter()
                        .map(|(t, id)| (t.clone(), !(t == "B" && *id == 2)))
                        .collect(),
                )))
            }),
            solo: Box::new(|_| Ok((TestRun::Passed, TestResults::default()))),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        // 1 passes A then B: a survivor. 2 passes A, then B kills it.
        let reach = timed_reach(&[(1, &["A", "B"]), (2, &["A", "B"])]);
        let mut result = RunResult::default();
        let mut killers = HashMap::new();
        let packed = packed(
            &fake,
            &[1, 2],
            &reach,
            &options(std::path::Path::new(".")),
            &mut result,
            &mut killers,
        )
        .unwrap();
        assert!(packed.solo.is_empty());
        assert_eq!(result.outcomes[&1], Outcome::Survived);
        assert_eq!(result.outcomes[&2], Outcome::Killed);
        assert_eq!(killers[&2], ["B"]);
        // Round 1 spreads the load (1 gets A, 2 gets B) in one pack; 2 is
        // killed there, and only 1 goes on to its other test.
        assert_eq!(fake.calls(), ["p0 A,B", "p0 B"]);
    }

    #[test]
    fn a_mutant_cut_short_keeps_the_evidence_it_has_instead_of_an_outcome() {
        let reach = reach(&[(1, &["A", "B", "C"]), (2, &["A"])]);
        let mut result = RunResult::default();
        result.outcomes.insert(2, Outcome::Killed);
        let tried = HashMap::from([(1, set(&["A", "B"]))]);
        note_partial(&[1, 2], &reach, &tried, &mut result);
        assert_eq!(result.partial.get(&1), Some(&(2, 3)));
        assert!(!result.partial.contains_key(&2));
        assert!(!result.outcomes.contains_key(&1));
    }

    #[test]
    fn a_run_that_could_not_start_leaves_its_mutant_untested_not_the_run_failed() {
        let fake = Fake {
            pack: Box::new(|_| Err("could not clear the run folder".into())),
            solo: Box::new(|mutant| match mutant {
                1 => Err("could not clear the run folder".into()),
                _ => Ok((TestRun::Passed, passed(&["A"]))),
            }),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let reach = timed_reach(&[(1, &["A"]), (2, &["A"])]);
        let mut result = RunResult::default();
        let killers = test_mutants(
            &fake,
            &[1, 2],
            &reach,
            &options(std::path::Path::new(".")),
            &mut result,
        )
        .unwrap();
        // The pack could not run: both go solo, once each; 1's solo fails too.
        assert_eq!(result.untested.get(&1), Some(&Untested::RunError));
        assert_eq!(result.run_errors[&1], "could not clear the run folder");
        assert!(!result.outcomes.contains_key(&1));
        assert_eq!(result.outcomes[&2], Outcome::Survived);
        assert!(killers[&2].is_empty());
        assert!(!result.budget_hit(), "not the budget's doing");
        assert!(!result.partial.contains_key(&1), "no test of it ran");
        let solos = fake.calls().iter().filter(|c| c.contains("mutant")).count();
        assert_eq!(solos, 2);
    }

    #[test]
    fn a_mutant_tries_a_test_that_shares_its_run_before_one_that_cannot() {
        let mut both = reach(&[(0, &["q", "a"])]);
        both.times.insert("q".to_string(), Duration::from_millis(1));
        both.times
            .insert("a".to_string(), Duration::from_millis(50));
        let first = next_tests(&[0], &both, &HashMap::new(), 1);
        assert!(first.by_mutant[&0].contains("q"), "the quickest, normally");
        both.queuing.insert("q".to_string());
        let first = next_tests(&[0], &both, &HashMap::new(), 1);
        assert!(first.by_mutant[&0].contains("a"));
    }

    #[test]
    fn a_test_that_can_leave_state_behind_runs_with_no_other_mutated_test_of_its_file() {
        let mut leaky = reach(&[(0, &["q"]), (1, &["a"]), (2, &["b"]), (3, &["other"])]);
        for test in ["q", "a", "b"] {
            leaky.home.insert(test.to_string(), "s.spec.ts".to_string());
        }
        leaky
            .home
            .insert("other".to_string(), "t.spec.ts".to_string());
        leaky.queuing.insert("q".to_string());
        let (packs, _) = pack(&[0, 1, 2, 3], &leaky, 100);
        assert_eq!(packs.len(), 2, "{packs:?}");
        for p in &packs {
            let in_file: Vec<&String> = p
                .used
                .iter()
                .filter(|t| leaky.home[*t] == "s.spec.ts")
                .collect();
            assert!(
                in_file.len() == 1 || !in_file.iter().any(|t| *t == "q"),
                "{packs:?}"
            );
        }
        let with_other = packs.iter().find(|p| p.used.contains("other")).unwrap();
        assert!(
            with_other.used.len() > 1,
            "another file's test still shares a pack"
        );
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
        assert!(check_baseline(TestRun::Failed, &outcome(&[true, true, false]), None).is_ok());
        let half = check_baseline(TestRun::Failed, &outcome(&[true, false]), None).unwrap_err();
        assert!(half.contains("1 of 2 tests fail"), "{half}");
        assert!(check_baseline(TestRun::Passed, &HashMap::new(), None).is_err());
        // No test reported: the error quotes what the run's output said.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(exec::log_name(0)),
            "Building...\nsrc/a.spec.ts(4,1): error TS2532: Object is possibly 'undefined'.\n",
        )
        .unwrap();
        let said = check_baseline(TestRun::Failed, &HashMap::new(), Some(dir.path())).unwrap_err();
        assert!(said.contains("error TS2532"), "{said}");
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
        let first = next_tests(&[0, 1, 2, 3], &reach, &HashMap::new(), 1);
        let one = |id: usize| first.by_mutant[&id].iter().next().unwrap().clone();
        // Mutant 2 has only Fast; 0 and 1 share Fast too (half a second per
        // mutant is far less than the slow test); 3 takes Other.
        assert!(first.by_mutant.values().all(|tests| tests.len() == 1));
        assert_eq!(one(2), "Fast");
        assert_eq!(one(0), "Fast");
        assert_eq!(one(3), "Other");
        // The survivors' next round: only tests they have not tried.
        let tried = first.by_mutant.clone();
        let rest = next_tests(&[0, 2, 3], &reach, &tried, 2);
        assert_eq!(rest.by_mutant[&0], BTreeSet::from(["Slow".to_string()]));
        assert!(!rest.by_mutant.contains_key(&2), "nothing left for 2");
        assert_eq!(rest.by_mutant[&3], BTreeSet::from(["Slow".to_string()]));
        // Two at once, cheapest first.
        let both = next_tests(&[3], &reach, &HashMap::new(), 2);
        assert_eq!(both.by_mutant[&3].len(), 2);
        assert_eq!(
            next_tests(&[3], &reach, &HashMap::new(), 1).by_mutant[&3],
            BTreeSet::from(["Other".to_string()])
        );
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
        let found = resolve(
            &packs[0],
            &reach,
            &Some(TestResults::of(results)),
            &mut outcomes,
            &mut killers,
        );
        assert_eq!(outcomes[&0], Outcome::Killed);
        assert_eq!(killers[&0], ["B"]);
        // 1 passed its planned test: not a survivor yet, that is the round's call.
        assert!(!outcomes.contains_key(&1));
        assert_eq!(
            found,
            Resolved {
                passed: vec![1],
                undecided: vec![2],
                fresh: vec![],
                cut_short: vec![],
            }
        );
        assert_eq!(
            resolve(&packs[0], &reach, &None, &mut outcomes, &mut killers)
                .undecided
                .len(),
            3
        );
        // A failing test decides its mutant even when the run skipped the
        // mutant's other tests; without one, a missing test leaves it open.
        let skipped = self::reach(&[(0, &["A", "B"]), (1, &["C", "D"])]);
        let (packs, _) = pack(&[0, 1], &skipped, 100);
        let results: HashMap<String, bool> = [("A", false), ("C", true)]
            .iter()
            .map(|(t, p)| (t.to_string(), *p))
            .collect();
        let found = resolve(
            &packs[0],
            &skipped,
            &Some(TestResults::of(results)),
            &mut outcomes,
            &mut killers,
        );
        assert_eq!(outcomes[&0], Outcome::Killed);
        assert_eq!(found.undecided, [1]);
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
    fn a_failed_language_leaves_its_mutants_untested_and_a_run_of_only_it_fails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("web/src")).unwrap();
        // Vitest is wanted but not installed: the TypeScript run fails.
        std::fs::write(
            root.join("web/package.json"),
            r#"{"devDependencies":{"vitest":"3"}}"#,
        )
        .unwrap();
        let source = "export function f(a: number) {
  return a > 1;
}
";
        std::fs::write(root.join("web/src/a.ts"), source).unwrap();
        let wanted = crate::mutate::Wanted {
            file: "web/src/a.ts".into(),
            path: root.join("web/src/a.ts"),
            name: "f".into(),
            start_line: 1,
            end_line: 3,
            complexity: 2,
            reason: "selected",
            lines: None,
        };
        let (mut targets, _) =
            crate::mutate::targets(vec![wanted], &dog_food_core::mutate::Settings::default())
                .unwrap();
        number(&mut targets);
        let mut result = RunResult::default();
        let pending: Vec<&Target> = targets.iter().collect();
        failed(
            sandbox::Kind::Node,
            "no runner".into(),
            &pending,
            &mut result,
        );
        assert_eq!(result.untested.len(), targets[0].mutants.len());
        assert!(result.untested.values().all(|s| *s == Untested::Failed));
        assert!(!result.budget_hit());
        assert_eq!(
            result.failed,
            ["the TypeScript/JavaScript run failed: no runner"]
        );
        assert!(result.notes[0].starts_with("the TypeScript/JavaScript run failed"));
        // With no other language to report, the run is an error.
        let err = run(&mut targets, &options(root)).err().unwrap();
        assert!(
            err.starts_with("the TypeScript/JavaScript run failed"),
            "{err}"
        );
    }

    #[test]
    fn when_both_languages_fail_each_failure_is_told() {
        let mut result = RunResult::default();
        failed(
            sandbox::Kind::CSharp,
            "the sandbox does not build".into(),
            &[],
            &mut result,
        );
        failed(sandbox::Kind::Node, "no runner".into(), &[], &mut result);
        let err = answer(result, 0).err().unwrap();
        assert_eq!(
            err,
            "the C# run failed: the sandbox does not build\nthe TypeScript/JavaScript run failed: no runner"
        );
        // One language finished: its results are shown, the failure as a note.
        let mut result = RunResult::default();
        failed(
            sandbox::Kind::CSharp,
            "the sandbox does not build".into(),
            &[],
            &mut result,
        );
        assert!(answer(result, 1).is_ok());
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
