//! The C# engine: `dotnet build`, and dog-food's test workers, which keep a
//! test project loaded between runs and tell the mutant switches which test
//! is running.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use super::exec::{self, fresh_folder, write_plan, Located};
use super::projects::TestProject;
use super::sandbox::FileMutants;
use super::worker::{Pool, Reply, Report, Target, Worker};
use super::{
    check_baseline, failing_tests, progress, Budget, BuildError, Engine, Pack, PackRun, Reach,
    Round, SoloRun, TestResults, TestRun,
};
use crate::discover::absolute;

const WORKER_CS: &str = include_str!("../../assets/worker/DgfWorker.cs");
const WORKER_PROJECT: &str = include_str!("../../assets/worker/DgfWorker.csproj");
const MTP_CS: &str = include_str!("../../assets/mtp/DgfMtp.cs");
const MTP_PROJECT: &str = include_str!("../../assets/mtp/DgfMtp.csproj");
/// Longest a build or the baseline test run may take.
const HOUR: Duration = Duration::from_secs(3600);
/// Every test's time in a baseline, read by the next run to split its baseline evenly.
const TIMES: &str = "baseline-times.tsv";

#[derive(Clone)]
struct Project {
    path: PathBuf,
    /// The assembly its tests come from.
    assembly: String,
    /// The user's test filter for this project.
    filter: Option<String>,
}

pub struct Dotnet {
    sandbox: PathBuf,
    projects: Vec<Project>,
    /// A solution with every test project, so one `dotnet build` builds
    /// shared projects once and the rest in parallel.
    tests: PathBuf,
    /// A solution with the projects that have mutants.
    mutated: PathBuf,
    /// Fingerprint of the build settings; restore is skipped while the last
    /// successful build in this sandbox had the same.
    settings: u64,
    restored: AtomicBool,
    /// Whether the projects with mutants were restored: one that no test
    /// project uses is not part of the first build's restore.
    mutated_restored: AtomicBool,
    /// Extra `dotnet build` arguments: `-m:N`, `-lowPriority`.
    build_args: Vec<String>,
    /// dog-food's test workers, which keep a test project loaded between runs.
    workers: Pool,
    /// The baseline's `--jobs` places, across all test projects.
    slots: Slots,
    /// Test project (assembly) → what a worker serves, found after the first
    /// build; or why a test project cannot run.
    targets: OnceLock<Result<HashMap<String, Target>, String>>,
    /// Test project → workers the kill check retired as polluted.
    retired: Mutex<HashMap<String, usize>>,
    /// Test project → how much longer a run takes in a new worker than in
    /// a warm one (starting, finding the tests, loading and compiling).
    warmup: Mutex<HashMap<String, Duration>>,
    /// Mutant → the assembly (project file name) it is compiled into.
    owners: HashMap<usize, String>,
    /// Test runs at once (`--jobs`): the baseline splits across that many workers.
    jobs: usize,
    /// Each test's time in the last run's baseline.
    last_times: Mutex<HashMap<String, Duration>>,
    notes: Mutex<Vec<String>>,
    /// The projects with mutants.
    mutated_projects: Vec<PathBuf>,
    /// Test projects left out of the run: they do not build in the sandbox,
    /// or dog-food cannot run them.
    skipped: Mutex<Vec<Skipped>>,
}

/// A test project left out of the run.
struct Skipped {
    /// The projects it uses, itself included, by `project_key`.
    uses: HashSet<String>,
}

/// Where a sandbox records the build settings of its last successful build.
pub const RESTORED: &str = ".dgf-restored";

/// Polluted workers of one test project after which its kill check is off.
const RETIREMENTS: usize = 10;

impl Dotnet {
    /// Uses the test projects `chosen` (relative to the repository, which the
    /// sandbox mirrors).
    pub fn prepare(
        sandbox: &Path,
        files: &[FileMutants],
        chosen: &[TestProject],
        settings: u64,
        build_args: Vec<String>,
        jobs: usize,
    ) -> Result<Dotnet, String> {
        let projects: Vec<Project> = chosen
            .iter()
            .map(|t| Project {
                path: sandbox.join(&t.path),
                assembly: t.assembly.clone(),
                filter: t.filter.clone(),
            })
            .collect();
        let mutated: BTreeSet<PathBuf> = files
            .iter()
            .filter_map(|f| super::sandbox::project_file(&sandbox.join(&f.relative)))
            .collect();
        let tests = sandbox.join("dgf-tests.sln");
        write_if_changed(
            &tests,
            &solution(sandbox, projects.iter().map(|p| p.path.as_path())),
        )?;
        let mutated_projects: Vec<PathBuf> = mutated.iter().cloned().collect();
        let mutated_solution = sandbox.join("dgf-mutated.sln");
        write_if_changed(
            &mutated_solution,
            &solution(sandbox, mutated.iter().map(PathBuf::as_path)),
        )?;
        Ok(Dotnet {
            sandbox: sandbox.to_path_buf(),
            projects,
            tests,
            mutated: mutated_solution,
            settings,
            restored: AtomicBool::new(
                std::fs::read_to_string(sandbox.join(RESTORED))
                    .is_ok_and(|t| t == format!("{settings:016x}")),
            ),
            mutated_restored: AtomicBool::new(false),
            build_args,
            workers: Pool::new(exec::runs_folder(sandbox).join("workers")),
            slots: Slots::new(jobs.max(1)),
            targets: OnceLock::new(),
            retired: Mutex::new(HashMap::new()),
            warmup: Mutex::new(HashMap::new()),
            owners: owners(sandbox, files),
            jobs: jobs.max(1),
            last_times: Mutex::new(HashMap::new()),
            notes: Mutex::new(Vec::new()),
            mutated_projects,
            skipped: Mutex::new(Vec::new()),
        })
    }

    /// Runs the given projects (with a test filter each) for `ask` and
    /// collects every test's result; side by side when `parallel`.
    fn run(
        &self,
        runs: &[(&Project, Option<String>)],
        ask: Ask,
        limit: Duration,
        folder: &Path,
        parallel: bool,
    ) -> Result<(TestRun, Results), String> {
        let one = |(index, (project, filter)): (usize, &(&Project, Option<String>))| {
            let started = Instant::now();
            let target = self.target(project)?;
            let ran = self.in_worker(
                project,
                &target,
                filter.as_deref(),
                ask,
                limit,
                folder,
                index,
            )?;
            Ok::<_, String>((ran, project.assembly.clone(), started.elapsed()))
        };
        let ran: Vec<Result<ProjectRun, String>> = if parallel {
            runs.par_iter().enumerate().map(one).collect()
        } else {
            runs.iter().enumerate().map(one).collect()
        };
        let mut status = TestRun::Passed;
        let mut results = Results::default();
        for run in ran {
            let ((one_status, one_results), assembly, wall) = run?;
            status = worst(status, one_status);
            results.add(one_results);
            results.walls.insert(assembly, wall);
        }
        // What a planned run reported before it timed out still counts.
        if status == TestRun::TimedOut && !matches!(ask, Ask::Plan(..)) {
            return Ok((status, Results::default()));
        }
        Ok((status, results))
    }

    /// One project's run in a worker: a warm one from the pool, or a new
    /// one with the mutant on from the start for a mutant reached outside
    /// any test (static setup runs once per process). A worker that timed
    /// out or crashed is stopped. One with the mutant on from the start that
    /// cannot start was crashed by its mutant, as the baseline's started.
    #[allow(clippy::too_many_arguments)]
    fn in_worker(
        &self,
        project: &Project,
        target: &Target,
        filter: Option<&str>,
        ask: Ask,
        limit: Duration,
        folder: &Path,
        index: usize,
    ) -> Result<(TestRun, Results), String> {
        let pool = &self.workers;
        let mut worker = match ask {
            Ask::Fresh(mutant, _) => {
                match pool.start(target, &[("DGF_MUTANT", mutant.to_string())]) {
                    Ok(worker) => worker,
                    Err(why) => {
                        debug!("worker", "mutant {mutant} stopped a new worker: {why}");
                        return Ok((TestRun::Crashed, Results::default()));
                    }
                }
            }
            // A worker that would not start is tried once more: a machine
            // short of memory or a build server shutting down can pass.
            _ => pool
                .take(target)
                .or_else(|_| pool.take(target))
                .map_err(|why| format!("{}: {why}", project.assembly))?,
        };
        if let (Ask::Reach(reached), true) = (ask, splittable(filter)) {
            return self.reach_split(
                project, target, worker, filter, reached, limit, folder, index,
            );
        }
        let check = self.checks(&project.assembly);
        let request = request_for(ask, &project.assembly, filter, folder, index, check)?;
        let limit = if worker.warm {
            limit
        } else {
            limit + self.warmup_of(&project.assembly) * 2
        };
        let reply = worker.request(&request, limit);
        let fresh = matches!(ask, Ask::Fresh(..));
        Ok(self.finish(project, target, worker, reply, fresh, ask, folder, index))
    }

    /// Runs the baseline of `project` in batches of whole test classes, in as
    /// many workers as its tests are worth while a slot is free: a slot a small
    /// project has finished with goes to a big one. They stay warm for the mutants.
    #[allow(clippy::too_many_arguments)]
    fn reach_split(
        &self,
        project: &Project,
        target: &Target,
        mut first: Worker,
        filter: Option<&str>,
        reached: &Path,
        limit: Duration,
        folder: &Path,
        index: usize,
    ) -> Result<(TestRun, Results), String> {
        let filter_file = folder.join(format!("filter-{index}.txt"));
        write_file(&filter_file, filter.unwrap_or(""))?;
        let ask = Ask::Reach(reached);
        let listed: Vec<String> =
            match first.request(&format!("list\t{}", filter_file.display()), limit) {
                Reply::Done(report) => report.rows.into_iter().map(|r| r.test).collect(),
                other => {
                    return Ok(self.finish(project, target, first, other, false, ask, folder, index))
                }
            };
        let times = self
            .last_times
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default();
        let workers = baseline_workers(&listed, &times, self.jobs);
        let batches = split_by_class(&listed, workers * BATCHES_PER_WORKER, &times);
        debug!(
            "baseline",
            "{}: {} tests, up to {workers} workers (jobs {}), {} batches, {} tests timed last run",
            project.assembly,
            listed.len(),
            self.jobs,
            batches.len(),
            times.len()
        );
        if workers < 2 || batches.len() < 2 {
            let _slot = self.slots.hold();
            let request = format!("tests\t{}\t{}", filter_file.display(), reached.display());
            let reply = first.request(&request, limit);
            return Ok(self.finish(project, target, first, reply, false, ask, folder, index));
        }
        let shared = BaselineShare {
            project,
            target,
            ask,
            reached,
            limit,
            folder,
            index,
            batches: Mutex::new(batches.into_iter().enumerate().collect()),
            first: Mutex::new(Some(first)),
            stopped: AtomicBool::new(false),
        };
        self.share_baseline(&shared, workers)
    }

    /// Runs the batches of `shared` in up to `workers` workers.
    fn share_baseline(
        &self,
        shared: &BaselineShare,
        workers: usize,
    ) -> Result<(TestRun, Results), String> {
        let (project, target) = (shared.project, shared.target);
        let ran: Vec<Result<WorkerShare, String>> = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..workers)
                .map(|_| scope.spawn(|| self.baseline_worker(shared)))
                .collect();
            threads
                .into_iter()
                .map(|t| {
                    t.join()
                        .unwrap_or_else(|_| Err("a test worker thread failed".into()))
                })
                .collect()
        });
        if let Some(worker) = shared.first.lock().ok().and_then(|mut f| f.take()) {
            self.release(target, worker, false);
        }
        let mut status = TestRun::Passed;
        let mut results = Results::default();
        let mut warmup = Duration::ZERO;
        for (number, ran) in ran.into_iter().enumerate() {
            let share = ran?;
            status = worst(status, share.status);
            let tests: Duration = share.results.times.values().sum();
            debug!(
                "baseline",
                "{} worker {number}: {:?}, {} batches in {:.1} s, its tests {:.1} s",
                project.assembly,
                share.status,
                share.batches,
                share.wall.as_secs_f64(),
                tests.as_secs_f64()
            );
            warmup = warmup.max(share.wall.saturating_sub(tests));
            results.add(share.results);
        }
        if let Ok(mut known) = self.warmup.lock() {
            known.insert(project.assembly.clone(), warmup);
        }
        Ok((status, results))
    }

    /// One worker's part of a baseline: once one of the run's slots is free,
    /// runs batches of `shared` until none is left. It starts no worker when
    /// the others finished them first, and stops when a worker timed out or crashed.
    fn baseline_worker(&self, shared: &BaselineShare) -> Result<WorkerShare, String> {
        let _slot = self.slots.hold();
        let started = Instant::now();
        let mut share = WorkerShare {
            status: TestRun::Passed,
            results: Results::default(),
            batches: 0,
            wall: Duration::ZERO,
        };
        while !shared.stopped.load(Ordering::Relaxed) {
            let Some((number, names)) = shared.batches.lock().ok().and_then(|mut b| b.pop_front())
            else {
                break;
            };
            let warm = shared.first.lock().ok().and_then(|mut f| f.take());
            let mut worker = match warm {
                Some(worker) => worker,
                None => self.workers.take(shared.target)?,
            };
            let file = shared
                .folder
                .join(format!("names-{}-{number}.txt", shared.index));
            write_file(&file, &names.join("\n"))?;
            let request = format!("names\t{}\t{}", file.display(), shared.reached.display());
            let reply = worker.request(&request, shared.limit);
            let (status, results) = self.finish(
                shared.project,
                shared.target,
                worker,
                reply,
                false,
                shared.ask,
                shared.folder,
                shared.index,
            );
            share.status = worst(share.status, status);
            share.results.add(results);
            share.batches += 1;
            if matches!(status, TestRun::TimedOut | TestRun::Crashed) {
                shared.stopped.store(true, Ordering::Relaxed);
            }
        }
        share.wall = started.elapsed();
        Ok(share)
    }

    /// A worker's reply as a run's status and results; a worker that timed
    /// out or crashed is stopped, a fresh one too, the others go back to the pool.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &self,
        project: &Project,
        target: &Target,
        worker: Worker,
        reply: Reply,
        fresh: bool,
        ask: Ask,
        folder: &Path,
        index: usize,
    ) -> (TestRun, Results) {
        match reply {
            Reply::Done(report) => {
                let status = if report.rows.iter().all(|r| r.passed) {
                    TestRun::Passed
                } else {
                    TestRun::Failed
                };
                if report.polluted.is_empty() {
                    self.release(target, worker, fresh);
                } else {
                    self.retire(project, worker);
                }
                (
                    status,
                    self.results(project, report, matches!(ask, Ask::Plan(..))),
                )
            }
            Reply::TimedOut { report, .. } => {
                worker.stop();
                let planned = matches!(ask, Ask::Plan(..));
                let running = report.running.clone();
                let mut results = self.results(project, report, planned);
                results.running = running;
                if planned {
                    // The worker never got to say which mutants took effect.
                    results.activated =
                        activated_in(&folder.join(format!("plan-{index}.txt.activated")));
                }
                (TestRun::TimedOut, results)
            }
            Reply::Crashed(report) => {
                let _ = std::fs::copy(&worker.log, folder.join(exec::log_name(index)));
                worker.stop();
                (TestRun::Crashed, self.results(project, report, false))
            }
        }
    }

    /// Stops a worker the kill check found polluted (a test fails in it
    /// without its mutant): its state is not to be trusted again. After
    /// `RETIREMENTS` of a project's workers, the check is turned off for
    /// the project, with a note: its tests are not idempotent, and the
    /// check would retire a worker per kill.
    fn retire(&self, project: &Project, worker: Worker) {
        worker.stop();
        let Ok(mut retired) = self.retired.lock() else {
            return;
        };
        let count = retired.entry(project.assembly.clone()).or_insert(0);
        *count += 1;
        debug!(
            "kill check",
            "{}: a worker retired, {count} so far: a test failed in it without its mutant",
            project.assembly
        );
        if *count == RETIREMENTS {
            if let Ok(mut notes) = self.notes.lock() {
                notes.push(format!(
                    "{}: the kill check retired {RETIREMENTS} of its workers (a test failed again without its mutant each time), so its tests are not idempotent and the check is off for it from here on; its kills are not checked against process state",
                    project.assembly
                ));
            }
        }
    }

    /// Whether `assembly`'s plan runs still get the kill check.
    fn checks(&self, assembly: &str) -> bool {
        self.retired
            .lock()
            .is_ok_and(|r| r.get(assembly).is_none_or(|n| *n < RETIREMENTS))
    }

    /// A project in a worker starts once: what its first run took besides its
    /// tests is warm-up, which only a new worker pays again; its later runs
    /// have no startup.
    fn warm_startup(&self, mut startup: HashMap<String, Duration>) -> HashMap<String, Duration> {
        let warm: Vec<String> = self
            .projects
            .iter()
            .filter(|p| self.target(p).is_ok())
            .map(|p| p.assembly.clone())
            .collect();
        let Ok(mut warmup) = self.warmup.lock() else {
            return startup;
        };
        for assembly in warm {
            if let Some(time) = startup.insert(assembly.clone(), Duration::ZERO) {
                warmup.entry(assembly).or_insert(time);
            }
        }
        startup
    }

    /// A fresh worker stops (its statics hold the mutant); the others go back to the pool.
    fn release(&self, target: &Target, worker: Worker, fresh: bool) {
        if fresh {
            worker.stop();
        } else {
            self.workers.give(target, worker);
        }
    }

    /// The worker's report as results; `planned` runs
    /// report which mutants took effect.
    fn results(&self, project: &Project, report: Report, planned: bool) -> Results {
        let sandbox = absolute(&self.sandbox);
        let mut results = Results {
            polluted: report.polluted,
            activated: report.activated,
            activation_known: planned,
            ..Results::default()
        };
        for row in report.rows {
            results
                .home
                .insert(row.test.clone(), project.assembly.clone());
            if let Some(file) = (!row.file.is_empty())
                .then(|| relative_file(&sandbox, &row.file))
                .flatten()
            {
                results.files.insert(row.test.clone(), file);
            }
            results.times.insert(row.test.clone(), row.time);
            results.outcome.insert(row.test, row.passed);
        }
        results
    }

    /// What a worker serves for `project`.
    fn target(&self, project: &Project) -> Result<Target, String> {
        self.targets()?
            .get(&project.assembly)
            .cloned()
            .ok_or_else(|| format!("{}: no build output found", project.assembly))
    }

    fn targets(&self) -> Result<&HashMap<String, Target>, String> {
        self.targets
            .get_or_init(|| self.find_targets())
            .as_ref()
            .map_err(Clone::clone)
    }

    /// The test projects' built assemblies, once the first build made them,
    /// and the workers they need. `reach` finds them before its test runs
    /// start; plain threads, not the thread pool the runs use, so no run
    /// waits on itself.
    fn find_targets(&self) -> Result<HashMap<String, Target>, String> {
        let found: Vec<(&Project, Result<Found, String>)> = std::thread::scope(|scope| {
            let lookups: Vec<_> = self
                .projects
                .iter()
                .map(|p| (p, scope.spawn(move || find_target(&p.path))))
                .collect();
            lookups
                .into_iter()
                .map(|(p, lookup)| {
                    let found = lookup
                        .join()
                        .unwrap_or_else(|_| Err("looking up its build output failed".into()));
                    (p, found)
                })
                .collect()
        });
        if found.iter().all(|(_, target)| target.is_err()) {
            return Err(unservable(&found));
        }
        let targets = self.served(found);
        if targets.values().any(|t| t.mtp) {
            self.workers.set_mtp(mtp_program()?);
        }
        if targets.values().any(|t| !t.mtp) {
            self.workers.set_vstest(worker_program()?);
        }
        Ok(targets)
    }

    /// The test projects a worker can serve, by assembly; the others are
    /// skipped, with a note saying why.
    fn served(&self, found: Vec<(&Project, Result<Found, String>)>) -> HashMap<String, Target> {
        let mut targets = HashMap::new();
        for (project, target) in found {
            match target {
                Ok(found) => {
                    self.note_target(&project.assembly, &found);
                    targets.insert(project.assembly.clone(), found.target);
                }
                Err(why) => self.skip(
                    &project.path,
                    format!(
                        "{} was skipped: dog-food cannot run it ({why}); the other test projects ran. Leave it out with --exclude-test-project to drop this note",
                        project.assembly
                    ),
                ),
            }
        }
        targets
    }

    /// Says what was found for a test project, and readies its test host.
    fn note_target(&self, assembly: &str, found: &Found) {
        let host = if found.target.mtp {
            " (testing platform)"
        } else {
            ""
        };
        debug!(
            "targets",
            "{assembly}: {}{host}",
            found.target.dll.display()
        );
        if let Some(framework) = &found.chosen {
            if let Ok(mut notes) = self.notes.lock() {
                notes.push(format!(
                    "{assembly} has several target frameworks: its tests ran on {framework}"
                ));
            }
        }
        if found.target.mtp {
            xunit_one_at_a_time(&found.target.dll);
        }
    }

    /// The runs whose test project can see `mutant`: its build output holds
    /// the mutant's assembly. A test project that does not reference that
    /// assembly cannot notice the mutant, however its tests run. When no
    /// output holds an assembly of that name (another AssemblyName), nothing
    /// is left out.
    fn seeing<'a>(
        &self,
        mutant: usize,
        runs: Vec<(&'a Project, Option<String>)>,
    ) -> Vec<(&'a Project, Option<String>)> {
        let Some(owner) = self.owners.get(&mutant) else {
            return runs;
        };
        let sees = |project: &Project| {
            self.target(project).ok().map(|t| {
                t.dll
                    .parent()
                    .is_some_and(|dir| dir.join(format!("{owner}.dll")).is_file())
            })
        };
        let seen: Vec<Option<bool>> = runs.iter().map(|(p, _)| sees(p)).collect();
        if seen.contains(&Some(false)) && seen.contains(&Some(true)) {
            runs.into_iter()
                .zip(seen)
                .filter(|(_, sees)| *sees != Some(false))
                .map(|(run, _)| run)
                .collect()
        } else {
            runs
        }
    }

    fn warmup_of(&self, assembly: &str) -> Duration {
        self.warmup
            .lock()
            .ok()
            .and_then(|w| w.get(assembly).copied())
            .unwrap_or(Duration::from_secs(30))
    }

    /// The projects that hold `tests`; every project with the user's filter
    /// when `tests` is `None`.
    fn runs_for<'a>(
        &'a self,
        tests: Option<&BTreeSet<String>>,
        reach: &Reach,
    ) -> Vec<(&'a Project, Option<String>)> {
        let Some(tests) = tests else {
            return self
                .projects
                .iter()
                .filter(|p| self.serves(p))
                .map(|p| (p, p.filter.clone()))
                .collect();
        };
        let homes: BTreeSet<&str> = tests
            .iter()
            .map(|test| reach.home.get(test).map_or("", String::as_str))
            .collect();
        self.projects
            .iter()
            .filter(|p| homes.contains(p.assembly.as_str()) && self.serves(p))
            .map(|p| (p, None))
            .collect()
    }

    /// Whether `project` runs: false for one skipped because dog-food cannot run it.
    fn serves(&self, project: &Project) -> bool {
        match self.targets.get() {
            Some(Ok(targets)) => targets.contains_key(&project.assembly),
            _ => true,
        }
    }

    /// Leaves `project` out of the run, and says why.
    fn skip(&self, project: &Path, note: String) {
        if let Ok(mut skipped) = self.skipped.lock() {
            skipped.push(Skipped {
                uses: uses(project),
            });
        }
        if let Ok(mut notes) = self.notes.lock() {
            notes.push(note);
        }
    }

    /// Each test's source files: the one the test adapter reports, else every
    /// file in the test projects that declares the test's class.
    fn test_files(&self, results: &Results) -> HashMap<String, Vec<String>> {
        let classes = self.class_index();
        results
            .outcome
            .keys()
            .filter_map(|test| {
                let files = match results.files.get(test) {
                    Some(file) => vec![file.clone()],
                    None => classes.get(class_of(test))?.clone(),
                };
                Some((test.clone(), files))
            })
            .collect()
    }

    /// Class name → the files (relative to the sandbox) of the test projects
    /// that declare it.
    fn class_index(&self) -> HashMap<String, Vec<String>> {
        let mut index: HashMap<String, Vec<String>> = HashMap::new();
        let sandbox = absolute(&self.sandbox);
        let folders = self.projects.iter().filter_map(|p| p.path.parent());
        for file in folders.flat_map(cs_files) {
            let Some(relative) = relative_file(&sandbox, &file.to_string_lossy()) else {
                continue;
            };
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            for name in declared_types(&text) {
                index.entry(name).or_default().push(relative.clone());
            }
        }
        index
    }

    /// Builds `target`; `alone` skips its project references, which an
    /// earlier build of everything left up to date. A build without a
    /// restore that finds a project's restore output missing (the sandbox's
    /// last restore did not cover it, or its `obj` folder was cleared) is
    /// restored and built again.
    fn build_target(
        &self,
        target: &Path,
        restore: bool,
        alone: bool,
        budget: Budget,
    ) -> Result<(), BuildError> {
        let built = self.build_once(target, restore, alone, budget);
        if !restore && matches!(&built, Err(BuildError::Errors(e)) if needs_restore(e)) {
            debug!(
                "build",
                "{}: a project's restore output is missing; restoring",
                target.display()
            );
            return self.build_once(target, true, alone, budget);
        }
        built
    }

    fn build_once(
        &self,
        target: &Path,
        restore: bool,
        alone: bool,
        budget: Budget,
    ) -> Result<(), BuildError> {
        let mut args = vec!["build", "-nologo", "-v", "q", "-clp:NoSummary"];
        args.extend(SANDBOX_BUILD);
        if !restore {
            args.push("--no-restore");
        }
        if alone {
            args.push("--no-dependencies");
        }
        let mut command = dotnet(&args);
        command.args(&self.build_args);
        command.arg(target);
        let started = Instant::now();
        let output =
            exec::output(command, budget.limit(HOUR)).map_err(|e| BuildError::Errors(vec![e]))?;
        let took = started.elapsed().as_secs_f64();
        let Some((passed, text)) = output else {
            debug!(
                "build",
                "{} out of time after {took:.1} s",
                target.display()
            );
            return Err(BuildError::OutOfTime);
        };
        if passed {
            debug!(
                "build",
                "{} built in {took:.1} s{}{}",
                target.display(),
                if restore { ", with a restore" } else { "" },
                if alone {
                    ", without its references"
                } else {
                    ""
                }
            );
            return Ok(());
        }
        let found: Vec<String> = text
            .lines()
            .filter(|l| l.contains(": error "))
            .map(str::to_string)
            .collect();
        debug!(
            "build",
            "{} failed in {took:.1} s{}: {} errors",
            target.display(),
            if alone {
                ", without its references"
            } else {
                ""
            },
            found.len()
        );
        for error in found.iter().take(20) {
            debug!("build", "{error}");
        }
        Err(BuildError::Errors(if found.is_empty() {
            vec![text]
        } else {
            found
        }))
    }
}

/// Sandbox builds leave out what only slows them down or turns warnings into
/// errors: analyzers (source generators still run), warnings as errors,
/// code-style enforcement, and the NuGet vulnerability audit.
const SANDBOX_BUILD: &[&str] = &[
    // Errors name files by their full path, so dgf can tell which mutant
    // broke the build even where a repository turns full paths off.
    "-p:GenerateFullPaths=true",
    "-p:RunAnalyzers=false",
    "-p:TreatWarningsAsErrors=false",
    "-p:EnforceCodeStyleInBuild=false",
    "-p:NuGetAudit=false",
];

/// Errors from the C# compiler, which only runs once the restore worked.
fn compiler_errors(errors: &[String]) -> bool {
    errors.iter().any(|e| e.contains(": error CS"))
}

/// NETSDK1004: a project's restore output (`project.assets.json`) is not there.
fn needs_restore(errors: &[String]) -> bool {
    errors.iter().any(|e| e.contains("error NETSDK1004"))
}

/// CS0006: a referenced project's assembly is not there.
fn missing_reference(errors: &[String]) -> bool {
    errors.iter().any(|e| e.contains("error CS0006"))
}

fn worst(a: TestRun, b: TestRun) -> TestRun {
    match (a, b) {
        (TestRun::TimedOut, _) | (_, TestRun::TimedOut) => TestRun::TimedOut,
        (TestRun::Crashed, _) | (_, TestRun::Crashed) => TestRun::Crashed,
        (TestRun::Failed, _) | (_, TestRun::Failed) => TestRun::Failed,
        _ => TestRun::Passed,
    }
}

/// Whether a build got past its restore: it worked, or only the compiler failed.
fn restored(built: &Result<(), BuildError>) -> bool {
    built.is_ok() || matches!(built, Err(BuildError::Errors(e)) if compiler_errors(e))
}

impl Dotnet {
    /// The projects with mutants, built without their references.
    fn build_mutated(&self, budget: Budget) -> Result<(), BuildError> {
        let restore = !self.mutated_restored.load(Ordering::Relaxed);
        let mut built = self.build_target(&self.mutated, restore, true, budget);
        // Built without their references, the mutated projects compile
        // side by side: one whose mutants broke the last round has no
        // reference assembly yet when a project using it reads it. Built
        // with references, it comes first.
        if matches!(&built, Err(BuildError::Errors(e)) if missing_reference(e)) {
            built = self.build_target(&self.mutated, false, false, budget);
        }
        if restored(&built) {
            self.mutated_restored.store(true, Ordering::Relaxed);
        }
        built
    }
}

impl Engine for Dotnet {
    /// `All` builds every test project (and what they use); `Mutated` only
    /// the projects with mutants, which is where a mutant can break the build.
    fn build(&self, round: Round, budget: Budget) -> Result<(), BuildError> {
        if round == Round::Mutated {
            return self.build_mutated(budget);
        }
        let restore = !self.restored.load(Ordering::Relaxed);
        let built = self.build_target(&self.tests, restore, false, budget);
        // Compiler errors come after a restore that worked: the builds after
        // a mutant broke this one need not restore again.
        if restored(&built) {
            self.restored.store(true, Ordering::Relaxed);
            let _ = std::fs::write(
                self.sandbox.join(RESTORED),
                format!("{:016x}", self.settings),
            );
        }
        built
    }

    fn locate(&self, files: &[FileMutants], errors: &[String]) -> Located {
        exec::locate(&self.sandbox, &self.sandbox, files, errors)
    }

    fn reach(&self, budget: Budget) -> Result<Option<Reach>, String> {
        let last = exec::runs_folder(&self.sandbox)
            .join("baseline")
            .join(TIMES);
        if let Ok(mut times) = self.last_times.lock() {
            *times = read_times(&last);
        }
        let folder = fresh_folder(&self.sandbox, "baseline")?;
        let reached = folder.join("reach");
        std::fs::create_dir_all(&reached).map_err(|err| err.to_string())?;
        let started = Instant::now();
        self.targets()?;
        let runs = self.runs_for(None, &Reach::default());
        let (status, results) = self.run(
            &runs,
            Ask::Reach(&reached),
            budget.limit(HOUR),
            &folder,
            true,
        )?;
        if status == TestRun::TimedOut && budget.spent() {
            return Ok(None);
        }
        check_baseline(status, &results.outcome, Some(&folder))?;
        let startup = self.warm_startup(startup(&results.walls, &results.home, &results.times));
        let mut reach = Reach {
            failing: failing_tests(&results.outcome),
            files: self.test_files(&results),
            startup,
            home: results.home,
            times: results.times,
            duration: started.elapsed(),
            ..Reach::default()
        };
        record_reach(
            &mut reach,
            &exec::files_of(&reached, "reach-"),
            &exec::lines_of(&reached, "pending-"),
        );
        (reach.probed, reach.changed) = checked(&exec::lines_of(&reached, "checked-"));
        write_times(&folder.join(TIMES), &reach);
        Ok(Some(reach))
    }

    fn pack(
        &self,
        pack: &Pack,
        reach: &Reach,
        limit: Duration,
        tag: &str,
    ) -> Result<PackRun, String> {
        let folder = fresh_folder(&self.sandbox, tag)?;
        let tests: BTreeSet<String> = pack.plan.iter().map(|(t, _)| t.clone()).collect();
        let runs = self.runs_for(Some(&tests), reach);
        let grouped = by_project(&pack.plan, reach);
        let (status, results) = self.run(&runs, Ask::Plan(&grouped), limit, &folder, false)?;
        let results = results.into_test_results();
        // Nothing says which test was running: nothing to go on.
        if status == TestRun::TimedOut && results.running.is_empty() {
            return Ok(None);
        }
        Ok(Some(TestResults {
            timed_out: status == TestRun::TimedOut,
            ..results
        }))
    }

    fn solo(
        &self,
        mutant: usize,
        reach: &Reach,
        fresh: bool,
        limit: Duration,
        tag: &str,
    ) -> Result<SoloRun, String> {
        let folder = fresh_folder(&self.sandbox, tag)?;
        if fresh || reach.outside.contains(&mutant) {
            // A new process with the mutant on from the start, which stops
            // at the first failing test.
            let ordered = fresh_order(mutant, reach);
            let all: BTreeSet<String> = ordered.values().flatten().cloned().collect();
            let runs = self.seeing(mutant, self.runs_for(Some(&all), reach));
            let (status, results) =
                self.run(&runs, Ask::Fresh(mutant, &ordered), limit, &folder, false)?;
            return Ok((status, results.into_test_results()));
        }
        // Every test that reaches it, each with the mutant, in a warm process.
        let tests = reach.by_mutant.get(&mutant);
        let runs = self.runs_for(tests, reach);
        let entries: Vec<(String, usize)> = tests
            .into_iter()
            .flatten()
            .map(|t| (t.clone(), mutant))
            .collect();
        let grouped = by_project(&entries, reach);
        let (status, results) = self.run(&runs, Ask::Plan(&grouped), limit, &folder, false)?;
        Ok((status, results.into_test_results()))
    }

    fn checker(&self, files: &[FileMutants]) -> Option<super::check::Checker> {
        let started = Instant::now();
        let folder = exec::runs_folder(&self.sandbox).join("originals");
        let pairs: Vec<(PathBuf, &str)> = files
            .iter()
            .map(|f| (self.sandbox.join(&f.relative), f.source.as_str()))
            .collect();
        let checked = super::check::originals(&folder, &pairs).and_then(|pairs| {
            super::check::Checker::start(dog_food_dup::semantic::command(None), &pairs)
        });
        match checked {
            Ok(checker) => {
                debug!(
                    "build",
                    "compile checks in memory are ready after {:.1} s",
                    started.elapsed().as_secs_f64()
                );
                Some(checker)
            }
            Err(err) => {
                debug!("build", "no compile checks in memory: {err}");
                None
            }
        }
    }

    fn test_projects(&self) -> Vec<String> {
        self.projects
            .iter()
            .map(|p| match &p.filter {
                Some(filter) => format!("{} ({filter})", p.path.display()),
                None => p.path.display().to_string(),
            })
            .collect()
    }

    fn crash_reason(&self, tag: &str) -> Option<String> {
        exec::crash_reason(&exec::runs_folder(&self.sandbox).join(tag))
    }

    fn notes(&self) -> Vec<String> {
        self.notes.lock().map(|n| n.clone()).unwrap_or_default()
    }

    fn drop_broken(&mut self, errors: &[String], budget: Budget) -> bool {
        let tests: Vec<PathBuf> = self.projects.iter().map(|p| p.path.clone()).collect();
        let broken = match named_in(errors, &tests, &self.mutated_projects) {
            Named::CodeUnderTest => return false,
            Named::Tests(broken) => broken,
            Named::Unknown => self.build_each(budget),
        };
        if broken.is_empty() || broken.len() == self.projects.len() {
            return false;
        }
        self.leave_out(&broken)
    }

    fn skipped_may_reach(&self, relative: &Path) -> bool {
        let Some(project) = super::sandbox::project_file(&self.sandbox.join(relative)) else {
            return false;
        };
        let key = project_key(&project);
        self.skipped
            .lock()
            .is_ok_and(|skipped| skipped.iter().any(|s| s.uses.contains(&key)))
    }
}

impl Dotnet {
    /// Builds each test project on its own, one after the other (they share
    /// projects): the ones that fail, each with its first error. None when
    /// the budget runs out.
    fn build_each(&self, budget: Budget) -> Vec<(usize, String)> {
        let restore = !self.restored.load(Ordering::Relaxed);
        let mut broken = Vec::new();
        for (index, project) in self.projects.iter().enumerate() {
            match self.build_target(&project.path, restore, false, budget) {
                Ok(()) => {}
                Err(BuildError::OutOfTime) => return Vec::new(),
                Err(BuildError::Errors(errors)) => {
                    broken.push((index, errors.into_iter().next().unwrap_or_default()));
                }
            }
        }
        broken
    }

    /// Leaves the `broken` test projects (index, first error) out of the
    /// run, says why, and rewrites the solution of the test projects.
    fn leave_out(&mut self, broken: &[(usize, String)]) -> bool {
        for (index, error) in broken {
            let project = &self.projects[*index];
            progress(&format!(
                "{} does not build in the sandbox; leaving it out",
                project.assembly
            ));
            self.skip(
                &project.path,
                format!(
                    "{} was skipped: it does not build in the sandbox, even with no mutant in it ({}); the other test projects ran",
                    project.assembly,
                    shorten(error)
                ),
            );
        }
        let dropped: HashSet<usize> = broken.iter().map(|(index, _)| *index).collect();
        self.projects = std::mem::take(&mut self.projects)
            .into_iter()
            .enumerate()
            .filter(|(index, _)| !dropped.contains(index))
            .map(|(_, project)| project)
            .collect();
        write_if_changed(
            &self.tests,
            &solution(
                &self.sandbox,
                self.projects.iter().map(|p| p.path.as_path()),
            ),
        )
        .is_ok()
    }
}

/// What the projects named in a failed build's errors say about it.
#[derive(Debug, PartialEq)]
enum Named {
    /// A project with mutants does not build: its code is what is tested.
    CodeUnderTest,
    /// Only test projects fail: (index, first error) of each.
    Tests(Vec<(usize, String)>),
    /// Some other project fails, or an error names none: building each test
    /// project tells which fail.
    Unknown,
}

/// Which projects a failed build's errors name. MSBuild ends an error line
/// with its project: `... [D:\a\A.csproj]`, or `[...A.csproj::TargetFramework=net8.0]`.
fn named_in(errors: &[String], tests: &[PathBuf], mutated: &[PathBuf]) -> Named {
    let mutated: HashSet<String> = mutated.iter().map(|p| project_key(p)).collect();
    let tests: Vec<String> = tests.iter().map(|p| project_key(p)).collect();
    let mut first: BTreeMap<usize, String> = BTreeMap::new();
    let mut other = false;
    for error in errors {
        let Some(named) = project_named(error) else {
            other = true;
            continue;
        };
        let key = project_key(Path::new(&named));
        if mutated.contains(&key) {
            return Named::CodeUnderTest;
        }
        match tests.iter().position(|t| *t == key) {
            Some(index) => {
                first.entry(index).or_insert_with(|| error.clone());
            }
            None => other = true,
        }
    }
    if other || first.is_empty() {
        return Named::Unknown;
    }
    Named::Tests(first.into_iter().collect())
}

fn project_named(error: &str) -> Option<String> {
    let inside = error.trim_end().strip_suffix(']')?;
    let open = inside.rfind('[')?;
    let named = inside[open + 1..].split("::").next()?.trim();
    named
        .to_ascii_lowercase()
        .ends_with("proj")
        .then(|| named.to_string())
}

/// A project path as MSBuild and dog-food may both write it: absolute,
/// one kind of slash, one case.
fn project_key(path: &Path) -> String {
    let text = absolute(path).to_string_lossy().replace('/', "\\");
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_lowercase()
}

/// The projects `project` uses through project references, itself included.
fn uses(project: &Path) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut waiting = vec![absolute(project)];
    while let Some(next) = waiting.pop() {
        if !seen.insert(project_key(&next)) {
            continue;
        }
        let text = std::fs::read_to_string(&next).unwrap_or_default();
        let folder = next.parent().unwrap_or(Path::new("")).to_path_buf();
        waiting.extend(
            project_references(&text)
                .into_iter()
                .map(|r| absolute(&folder.join(r))),
        );
    }
    seen
}

/// The `Include` of each `<ProjectReference>`, with `/` for slashes; ones
/// built from MSBuild properties are left out.
fn project_references(text: &str) -> Vec<String> {
    text.split("<ProjectReference")
        .skip(1)
        .filter_map(|tail| {
            let tag = &tail[..tail.find('>').unwrap_or(tail.len())];
            let start = tag.find("Include")?;
            let value = tag[start..].split('"').nth(1)?;
            (!value.contains("$(")).then(|| value.replace('\\', "/"))
        })
        .collect()
}

/// The first 300 characters of a compiler error.
fn shorten(error: &str) -> String {
    let error = error.trim();
    match error.char_indices().nth(300) {
        Some((at, _)) => format!("{}...", &error[..at]),
        None => error.to_string(),
    }
}

/// One project's part of a run: its status and results, its assembly, and how long it took.
type ProjectRun = ((TestRun, Results), String, Duration);

/// What a test run is for.
#[derive(Clone, Copy)]
enum Ask<'a> {
    /// No mutant; record which mutants each test reaches in the folder.
    Reach(&'a Path),
    /// Each planned test with its mutant, by test project.
    Plan(&'a HashMap<String, Vec<(String, usize)>>),
    /// One mutant on from the start, in a new process: a mutant reached
    /// outside any test, or one a warm process cannot be trusted on. The
    /// tests to run, by test project, in order; a worker stops at the first
    /// failure.
    Fresh(usize, &'a HashMap<String, Vec<String>>),
}

/// Mutant → the name of the project file (and so, as a rule, the assembly)
/// its source file belongs to.
fn owners(sandbox: &Path, files: &[FileMutants]) -> HashMap<usize, String> {
    let mut owners = HashMap::new();
    for file in files {
        let Some(project) = super::sandbox::project_file(&sandbox.join(&file.relative)) else {
            continue;
        };
        let name = project
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        for mutant in &file.mutants {
            owners.insert(mutant.id, name.clone());
        }
    }
    owners
}

/// Whether the worker can match `filter` itself: none, or conditions on
/// FullyQualifiedName and DisplayName only.
fn splittable(filter: Option<&str>) -> bool {
    filter.is_none_or(|f| {
        f.split(['|', '&']).all(|term| {
            let term = term.trim().trim_start_matches(['(', '!']);
            let name = term.split(['=', '~', '!']).next().unwrap_or("").trim();
            term.is_empty() || matches!(name, "FullyQualifiedName" | "DisplayName")
        })
    })
}

/// An error naming the test projects no worker can serve, if any.
/// Why none of the test projects can run.
fn unservable(found: &[(&Project, Result<Found, String>)]) -> String {
    let refused: Vec<String> = found
        .iter()
        .filter_map(|(project, target)| {
            target
                .as_ref()
                .err()
                .map(|why| format!("{}: {why}", project.assembly))
        })
        .collect();
    format!(
        "dog-food cannot run any of the test projects:\n  {}",
        refused.join("\n  ")
    )
}

/// How long a test project's tests must take to earn another baseline worker.
const WORTH_A_WORKER: Duration = Duration::from_secs(20);

/// Batches per baseline worker: enough that one slow batch leaves the others
/// to the workers that are free, few enough that requests cost nothing.
const BATCHES_PER_WORKER: usize = 4;

/// How many workers `tests` are worth in a baseline, at most `jobs`: by their
/// time in the last baseline, 50 ms each when they have none.
fn baseline_workers(tests: &[String], times: &HashMap<String, Duration>, jobs: usize) -> usize {
    let cost: Duration = tests
        .iter()
        .map(|test| {
            times
                .get(test)
                .copied()
                .unwrap_or(Duration::ZERO)
                .max(Duration::from_millis(50))
        })
        .sum();
    let worth = (cost.as_secs_f64() / WORTH_A_WORKER.as_secs_f64()).ceil() as usize;
    worth.clamp(1, jobs.max(1))
}

/// The places to run a baseline in, `--jobs` of them across all test projects.
struct Slots {
    free: Mutex<usize>,
    freed: Condvar,
}

impl Slots {
    fn new(count: usize) -> Slots {
        Slots {
            free: Mutex::new(count),
            freed: Condvar::new(),
        }
    }

    /// Waits for a free slot; it is free again when the result is dropped.
    fn hold(&self) -> Slot<'_> {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        while *free == 0 {
            free = self.freed.wait(free).unwrap_or_else(|e| e.into_inner());
        }
        *free -= 1;
        Slot(self)
    }
}

struct Slot<'a>(&'a Slots);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.0.freed.notify_one();
    }
}

/// What a test project's baseline workers share.
struct BaselineShare<'a> {
    project: &'a Project,
    target: &'a Target,
    ask: Ask<'a>,
    reached: &'a Path,
    limit: Duration,
    folder: &'a Path,
    index: usize,
    /// The batches no worker took yet, with their numbers.
    batches: Mutex<VecDeque<(usize, Vec<String>)>>,
    /// The warm worker that listed the tests, for whichever gets a slot first.
    first: Mutex<Option<Worker>>,
    stopped: AtomicBool,
}

/// What one baseline worker did.
struct WorkerShare {
    status: TestRun,
    results: Results,
    batches: usize,
    wall: Duration,
}

/// `tests` in at most `shards` groups of whole test classes (a class's
/// tests share fixtures), the slowest class first into the quickest group;
/// a test's time from the last baseline, 50 ms when it has none.
fn split_by_class(
    tests: &[String],
    shards: usize,
    times: &HashMap<String, Duration>,
) -> Vec<Vec<String>> {
    let cost = |test: &String| {
        times
            .get(test)
            .copied()
            .unwrap_or(Duration::ZERO)
            .max(Duration::from_millis(50))
    };
    let mut classes: BTreeMap<&str, (Duration, Vec<String>)> = BTreeMap::new();
    for test in tests {
        let name = test.split('(').next().unwrap_or(test);
        let class = classes
            .entry(name.rsplit_once('.').map_or(name, |(class, _)| class))
            .or_default();
        class.0 += cost(test);
        class.1.push(test.clone());
    }
    let mut ordered: Vec<(Duration, Vec<String>)> = classes.into_values().collect();
    ordered.sort_by_key(|(time, _)| std::cmp::Reverse(*time));
    let mut groups: Vec<(Duration, Vec<String>)> =
        vec![Default::default(); shards.min(ordered.len()).max(1)];
    for (time, class) in ordered {
        let quickest = groups
            .iter_mut()
            .min_by_key(|(total, _)| *total)
            .expect("one group");
        quickest.0 += time;
        quickest.1.extend(class);
    }
    groups
        .into_iter()
        .map(|(_, tests)| tests)
        .filter(|g| !g.is_empty())
        .collect()
}

/// The times `write_times` kept from the last baseline.
fn read_times(path: &Path) -> HashMap<String, Duration> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let ms = parts.next()?.parse().ok()?;
            let test = parts.nth(1)?;
            Some((test.to_string(), Duration::from_millis(ms)))
        })
        .collect()
}

/// Every test's time in the baseline, slowest first, and the slowest classes in the log.
fn write_times(path: &Path, reach: &Reach) {
    let mut tests: Vec<(&String, &Duration)> = reach.times.iter().collect();
    tests.sort_by(|a, b| b.1.cmp(a.1));
    let mut text = String::from("ms\tproject\ttest\n");
    let mut classes: HashMap<&str, Duration> = HashMap::new();
    for (test, time) in &tests {
        let project = reach.home.get(*test).map_or("", String::as_str);
        text.push_str(&format!("{}\t{project}\t{test}\n", time.as_millis()));
        let name = test.split('(').next().unwrap_or(test);
        *classes
            .entry(name.rsplit_once('.').map_or(name, |(c, _)| c))
            .or_default() += **time;
    }
    let _ = std::fs::write(path, text);
    let mut slowest: Vec<(&str, Duration)> = classes.into_iter().collect();
    slowest.sort_by_key(|(_, time)| std::cmp::Reverse(*time));
    for (class, time) in slowest.iter().take(10) {
        debug!(
            "baseline",
            "slow class {class}: {:.1} s",
            time.as_secs_f64()
        );
    }
    debug!("baseline", "every test's time: {}", path.display());
}

/// The worker request for `ask` in the run `folder`, with the files it names.
fn request_for(
    ask: Ask,
    assembly: &str,
    filter: Option<&str>,
    folder: &Path,
    index: usize,
    check: bool,
) -> Result<String, String> {
    if let Ask::Plan(by_project) = ask {
        let plan = folder.join(format!("plan-{index}.txt"));
        write_plan(
            &plan,
            by_project.get(assembly).map_or(&[][..], Vec::as_slice),
        )?;
        let nocheck = if check { "" } else { "\tnocheck" };
        return Ok(format!("plan\t{}{nocheck}", plan.display()));
    }
    if let Ask::Fresh(_, by_project) = ask {
        let names = folder.join(format!("kill-{index}.txt"));
        let ordered = by_project.get(assembly).map_or(&[][..], Vec::as_slice);
        write_file(&names, &ordered.join("\n"))?;
        return Ok(format!("kill\t{}", names.display()));
    }
    let filter_file = folder.join(format!("filter-{index}.txt"));
    write_file(&filter_file, filter.unwrap_or(""))?;
    Ok(match ask {
        Ask::Reach(reached) => format!("tests\t{}\t{}", filter_file.display(), reached.display()),
        _ => format!("tests\t{}", filter_file.display()),
    })
}

/// Makes xUnit v3 run one test at a time in the sandbox's build output, where
/// a worker's request holds many tests: the time limit and the kill check
/// count on it, and `--jobs` already spreads the runs over the processors.
/// Keeps the project's other runner settings. Other frameworks' requests hold
/// one test.
fn xunit_one_at_a_time(dll: &Path) {
    let Some(folder) = dll
        .parent()
        .filter(|f| f.join("xunit.v3.core.dll").is_file())
    else {
        return;
    };
    let own = dll.with_extension("xunit.runner.json");
    let file = if own.is_file() {
        own
    } else {
        folder.join("xunit.runner.json")
    };
    let mut settings = std::fs::read_to_string(&file)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    settings["parallelizeTestCollections"] = serde_json::json!(false);
    settings["maxParallelThreads"] = serde_json::json!(1);
    let _ = std::fs::write(&file, settings.to_string());
}

/// The "test<TAB>mutant" lines the switches wrote next to a plan for the
/// planned mutants that took effect.
fn activated_in(file: &Path) -> BTreeSet<(String, usize)> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (test, id) = line.rsplit_once('\t')?;
            Some((test.to_string(), id.trim().parse().ok()?))
        })
        .collect()
}

/// The tests a fresh run of `mutant` runs, by test project, in order: the
/// tests that reach it, quickest first; then, for a mutant reached in code
/// that runs once, the rest of the classes that code ran in, then every
/// other test. Tests that fail without mutants are left out. The run stops
/// at the first failure, so the likeliest killers go first.
fn fresh_order(mutant: usize, reach: &Reach) -> HashMap<String, Vec<String>> {
    let reaching: Vec<String> = reach
        .by_mutant
        .get(&mutant)
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    let mut ordered = by_time(reach, reaching);
    if reach.outside.contains(&mutant) {
        let (mates, rest) = around_static(mutant, reach, &ordered);
        ordered.extend(by_time(reach, mates));
        ordered.extend(by_time(reach, rest));
    }
    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
    for test in ordered {
        let home = reach.home.get(&test).cloned().unwrap_or_default();
        grouped.entry(home).or_default().push(test);
    }
    grouped
}

/// The other tests a mutant reached in code that runs once may be noticed
/// by: those of the classes that code first ran in, and the rest; `taken`
/// and the tests that fail anyway are left out.
fn around_static(mutant: usize, reach: &Reach, taken: &[String]) -> (Vec<String>, Vec<String>) {
    let classes: BTreeSet<String> = reach
        .outside_in
        .get(&mutant)
        .into_iter()
        .flatten()
        .map(|t| class_of_test(t))
        .collect();
    reach
        .home
        .keys()
        .filter(|t| !taken.contains(t) && !reach.failing.contains(*t))
        .cloned()
        .partition(|t| classes.contains(&class_of_test(t)))
}

/// `tests` quickest first (by their time in the baseline), ties by name.
fn by_time(reach: &Reach, mut tests: Vec<String>) -> Vec<String> {
    tests.sort_by_key(|t| (reach.times.get(t).copied().unwrap_or_default(), t.clone()));
    tests
}

/// The class of a test's full name, without the method and a theory row's arguments.
fn class_of_test(test: &str) -> String {
    let name = test.split('(').next().unwrap_or(test);
    name.rsplit_once('.').map_or(name, |(c, _)| c).to_string()
}

/// A plan's entries by the test project (assembly) of their test, quickest
/// test first: a worker that runs one test at a time skips a mutant's other
/// tests once one fails.
fn by_project(plan: &[(String, usize)], reach: &Reach) -> HashMap<String, Vec<(String, usize)>> {
    let mut grouped: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    for (test, mutant) in plan {
        let home = reach.home.get(test).cloned().unwrap_or_default();
        grouped
            .entry(home)
            .or_default()
            .push((test.clone(), *mutant));
    }
    for entries in grouped.values_mut() {
        entries.sort_by_key(|(test, _)| reach.times.get(test).copied().unwrap_or_default());
    }
    grouped
}

/// What a worker serves for a test project.
struct Found {
    target: Target,
    /// The framework dog-food chose among the project's several.
    chosen: Option<String>,
}

/// The built assembly of the test project at `project`, or why a worker
/// cannot serve it. A project with several target frameworks has no assembly
/// of its own: it runs on the newest one this machine can run.
fn find_target(project: &Path) -> Result<Found, String> {
    let first = msbuild_properties(project, None)?;
    if !first.path.is_empty() {
        return target_of(&first, None);
    }
    if first.frameworks.is_empty() {
        return Err("MSBuild names no build output for it".to_string());
    }
    let framework = newest_runnable(&first.frameworks, &installed_runtimes())?;
    let chosen = msbuild_properties(project, Some(&framework))?;
    target_of(&chosen, Some(framework))
}

fn target_of(properties: &Properties, chosen: Option<String>) -> Result<Found, String> {
    if version_of(&properties.framework).is_none() && properties.framework.starts_with("net4") {
        return Err(format!(
            "it targets .NET Framework ({}), which dog-food does not support",
            properties.framework
        ));
    }
    let target = Target::of(Path::new(&properties.path), properties.mtp)?;
    Ok(Found { target, chosen })
}

/// What MSBuild says of a test project.
struct Properties {
    path: String,
    framework: String,
    frameworks: String,
    mtp: bool,
}

fn msbuild_properties(project: &Path, framework: Option<&str>) -> Result<Properties, String> {
    let project_path = project.to_string_lossy();
    let mut args = vec![
        "msbuild",
        project_path.as_ref(),
        "-getProperty:TargetPath",
        "-getProperty:TargetFramework",
        "-getProperty:TargetFrameworks",
        "-getProperty:IsTestingPlatformApplication",
        "-nologo",
    ];
    let property = framework.map(|f| format!("-p:TargetFramework={f}"));
    args.extend(property.as_deref());
    let output = dotnet(&args)
        .output()
        .map_err(|err| format!("could not run dotnet: {err}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!("MSBuild could not read it: {}", text.trim()));
    }
    parse_properties(&text).ok_or_else(|| format!("MSBuild said: {}", text.trim()))
}

/// MSBuild's `{"Properties": {"TargetPath": ..., ...}}`.
fn parse_properties(json: &str) -> Option<Properties> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let properties = value.get("Properties")?;
    let text = |name: &str| properties[name].as_str().unwrap_or("").trim().to_string();
    Some(Properties {
        path: text("TargetPath"),
        framework: text("TargetFramework"),
        frameworks: text("TargetFrameworks"),
        mtp: text("IsTestingPlatformApplication").eq_ignore_ascii_case("true"),
    })
}

/// The version of a modern .NET target framework: `net10.0`,
/// `net8.0-windows`, `netcoreapp3.1`. .NET Framework (`net48`) and
/// `netstandard2.0` have none.
fn version_of(framework: &str) -> Option<(u32, u32)> {
    let name = framework.split('-').next()?;
    let number = name
        .strip_prefix("netcoreapp")
        .or_else(|| name.strip_prefix("net"))?;
    let (major, minor) = number.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// The newest of the `;`-separated `frameworks` whose major version is
/// among the `runtimes`, else the newest of them, whose worker says what is
/// missing when it does not start.
fn newest_runnable(frameworks: &str, runtimes: &[u32]) -> Result<String, String> {
    let mut modern: Vec<((u32, u32), &str)> = frameworks
        .split(';')
        .map(str::trim)
        .filter_map(|f| version_of(f).map(|v| (v, f)))
        .collect();
    modern.sort();
    modern
        .iter()
        .rev()
        .find(|(version, _)| runtimes.contains(&version.0))
        .or(modern.last())
        .map(|(_, framework)| framework.to_string())
        .ok_or_else(|| {
            format!(
                "none of its target frameworks ({frameworks}) is modern .NET, which dog-food needs"
            )
        })
}

/// The major versions of the .NET runtimes this machine has.
fn installed_runtimes() -> Vec<u32> {
    let output = dotnet(&["--list-runtimes"]).output();
    let text = output
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    runtimes_in(&text)
}

/// From lines "Microsoft.NETCore.App 10.0.13 [C:\\Program Files\\...]".
fn runtimes_in(listing: &str) -> Vec<u32> {
    listing
        .lines()
        .filter_map(|line| line.strip_prefix("Microsoft.NETCore.App "))
        .filter_map(|rest| rest.split('.').next()?.parse().ok())
        .collect()
}

// The assembly a test project builds (MSBuild's TargetPath), and whether it
/// is a Microsoft.Testing.Platform test application; `None` for a project
/// with several target frameworks, which has no TargetPath.
/// What a run reported: every test's result, its assembly and its file.
#[derive(Default)]
struct Results {
    outcome: HashMap<String, bool>,
    home: HashMap<String, String>,
    /// Test → its source file, relative to the sandbox (and so to the repository).
    files: HashMap<String, String>,
    /// Test → how long it ran (all rows of a theory together).
    times: HashMap<String, Duration>,
    /// Test project (assembly) → how long its run took.
    walls: HashMap<String, Duration>,
    /// Failed tests that fail without their mutant too (the kill check).
    polluted: BTreeSet<String>,
    /// The (test, mutant) pairs whose planned mutant took effect.
    activated: BTreeSet<(String, usize)>,
    /// Tests that had started and not reported when the run timed out.
    running: BTreeSet<String>,
    /// Whether the run reported activation at all (a plan run does).
    activation_known: bool,
}

impl Results {
    /// What a run reported, for the engine's caller.
    fn into_test_results(self) -> TestResults {
        TestResults {
            outcome: self.outcome,
            polluted: self.polluted,
            activated: self.activation_known.then_some(self.activated),
            running: self.running,
            ..TestResults::default()
        }
    }

    /// Adds another project's results.
    fn add(&mut self, other: Results) {
        self.outcome.extend(other.outcome);
        self.home.extend(other.home);
        self.files.extend(other.files);
        self.times.extend(other.times);
        self.polluted.extend(other.polluted);
        self.activated.extend(other.activated);
        self.running.extend(other.running);
        self.activation_known |= other.activation_known;
        self.walls.extend(other.walls);
    }
}

/// Per test project: its run's time without its tests' time (host start,
/// discovery, loading).
fn startup(
    walls: &HashMap<String, Duration>,
    home: &HashMap<String, String>,
    times: &HashMap<String, Duration>,
) -> HashMap<String, Duration> {
    walls
        .iter()
        .map(|(assembly, wall)| {
            let tests: Duration = times
                .iter()
                .filter(|(test, _)| home.get(*test) == Some(assembly))
                .map(|(_, time)| *time)
                .sum();
            (assembly.clone(), wall.saturating_sub(tests))
        })
        .collect()
}

/// The .cs files under `folder`, without build output.
fn cs_files(folder: &Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(folder)
        .standard_filters(false)
        .filter_entry(|e| !matches!(e.file_name().to_str(), Some("bin" | "obj")))
        .build()
        .flatten()
        .map(|e| e.into_path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "cs"))
        .collect()
}

/// Names declared with `class`, `record` or `struct` in C# source.
fn declared_types(text: &str) -> Vec<String> {
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    words
        .windows(2)
        .filter(|pair| matches!(pair[0], "class" | "record" | "struct"))
        .map(|pair| pair[1].to_string())
        .collect()
}

/// The class of a test: `Ns.Outer+Inner.Test(1, "a")` gives `Inner`.
fn class_of(test: &str) -> &str {
    let name = test.split('(').next().unwrap_or(test);
    let class = name.rsplit_once('.').map_or(name, |(class, _)| class);
    let class = class.rsplit(['.', '+']).next().unwrap_or(class);
    class.split('`').next().unwrap_or(class)
}

/// A test's source file relative to the sandbox, with `/` separators.
fn relative_file(sandbox: &Path, file: &str) -> Option<String> {
    let path = absolute(Path::new(file.trim()));
    let relative = path.strip_prefix(sandbox).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

/// Lines "test<TAB>mutant<TAB>same|changed" from the switches that worked
/// out a mutant's value next to the original's: the mutants checked, and
/// for each the tests in which its value differed.
fn checked(lines: &[String]) -> (HashSet<usize>, HashMap<usize, BTreeSet<String>>) {
    let mut probed = HashSet::new();
    let mut changed: HashMap<usize, BTreeSet<String>> = HashMap::new();
    for line in lines {
        let mut parts = line.split('\t');
        let (Some(test), Some(Ok(id)), Some(verdict)) = (
            parts.next(),
            parts.next().map(|id| id.trim().parse::<usize>()),
            parts.next(),
        ) else {
            continue;
        };
        probed.insert(id);
        if verdict.trim() == "changed" {
            changed.entry(id).or_default().insert(test.to_string());
        }
    }
    (probed, changed)
}

/// Reach lines "test<TAB>mutant", one file per test process; an empty test
/// means outside any test, in code that runs once per process, with the test
/// it ran in and the kind after the id. A static constructor ("cctor")
/// runs once per process, whichever tests it is first seen in, even one
/// shared by several classes' constructors: the mutant stays outside any
/// test and gets a process of its own. A lazy fill ("lazy") found in two
/// tests of one process runs for each test (a cache per test object) and
/// belongs to them.
///
/// `pending` lines "test<TAB>mutant" name the test that started next after a
/// mutant was reached with no test known: the test began before its start was
/// reported, or a class fixture ran. Such a mutant, never seen in code that
/// runs once, belongs to every test of that test's class.
fn record_reach(reach: &mut Reach, files: &[Vec<String>], pending: &[String]) {
    let mut once = RunOnce::default();
    let per_test: Vec<(usize, BTreeSet<String>)> = files
        .iter()
        .flat_map(|lines| read_reach(reach, lines, &mut once))
        .collect();
    for (id, tests) in per_test {
        if !once.cctor.contains(&id) {
            reach.outside.remove(&id);
        }
        reach.by_mutant.entry(id).or_default().extend(tests);
    }
    give_pending(reach, pending, &once.any);
}

/// The mutants reached in code that runs once, and those of them reached
/// in a static constructor.
#[derive(Default)]
struct RunOnce {
    any: HashSet<usize>,
    cctor: HashSet<usize>,
}

fn give_pending(reach: &mut Reach, pending: &[String], run_once: &HashSet<usize>) {
    for (test, id) in pending.iter().filter_map(|line| pending_line(line)) {
        if !run_once.contains(&id) && reach.outside.remove(&id) {
            give_to_class(reach, test, id);
        }
    }
}

/// One process's reach lines into `reach`; returns the lazy fills found
/// outside any test in two or more of its tests, which run for each test.
fn read_reach(
    reach: &mut Reach,
    lines: &[String],
    once: &mut RunOnce,
) -> Vec<(usize, BTreeSet<String>)> {
    let mut lazy_in: HashMap<usize, BTreeSet<String>> = HashMap::new();
    for line in lines {
        let mut parts = line.split('\t');
        let test = parts.next().unwrap_or("");
        let Some(Ok(id)) = parts.next().map(|id| id.trim().parse::<usize>()) else {
            continue;
        };
        if !test.is_empty() {
            reach
                .by_mutant
                .entry(id)
                .or_default()
                .insert(test.to_string());
            continue;
        }
        reach.outside.insert(id);
        let during = parts.next().map(str::trim).filter(|t| !t.is_empty());
        let Some(during) = during else {
            continue;
        };
        once.any.insert(id);
        reach
            .outside_in
            .entry(id)
            .or_default()
            .insert(during.to_string());
        // A line without a kind comes from a static constructor: the kind
        // "lazy" is the only one that can run per test.
        if parts.next().map(str::trim) == Some("lazy") {
            lazy_in.entry(id).or_default().insert(during.to_string());
        } else {
            once.cctor.insert(id);
        }
    }
    lazy_in.into_iter().filter(|(_, t)| t.len() > 1).collect()
}

fn pending_line(line: &str) -> Option<(&str, usize)> {
    let (test, id) = line.rsplit_once('\t')?;
    Some((test, id.trim().parse().ok()?))
}

/// `id` belongs to `test` and every other test of its class.
fn give_to_class(reach: &mut Reach, test: &str, id: usize) {
    let owner = class_of_test(test);
    let mates: Vec<String> = reach
        .home
        .keys()
        .filter(|t| class_of_test(t) == owner)
        .cloned()
        .collect();
    let tests = reach.by_mutant.entry(id).or_default();
    tests.insert(test.to_string());
    tests.extend(mates);
}

fn dotnet(args: &[&str]) -> Command {
    let mut command = Command::new("dotnet");
    command
        .args(args)
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .env("MSBUILDDISABLENODEREUSE", "1");
    command
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}

fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}

/// A classic .sln with `projects` (every SDK reads this format).
fn solution<'a>(sandbox: &Path, projects: impl Iterator<Item = &'a Path>) -> String {
    let mut entries = String::new();
    let mut configs = String::new();
    for path in projects {
        let relative = path
            .strip_prefix(sandbox)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('/', "\\");
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let guid = guid(&relative);
        entries.push_str(&format!(
            "Project(\"{{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}}\") = \"{name}\", \"{relative}\", \"{guid}\"\r\nEndProject\r\n"
        ));
        configs.push_str(&format!(
            "\t\t{guid}.Debug|Any CPU.ActiveCfg = Debug|Any CPU\r\n\t\t{guid}.Debug|Any CPU.Build.0 = Debug|Any CPU\r\n"
        ));
    }
    format!(
        "\r\nMicrosoft Visual Studio Solution File, Format Version 12.00\r\n{entries}Global\r\n\tGlobalSection(SolutionConfigurationPlatforms) = preSolution\r\n\t\tDebug|Any CPU = Debug|Any CPU\r\n\tEndGlobalSection\r\n\tGlobalSection(ProjectConfigurationPlatforms) = postSolution\r\n{configs}\tEndGlobalSection\r\nEndGlobal\r\n"
    )
}

/// A GUID made from a path, so the solution stays the same between runs.
fn guid(text: &str) -> String {
    let a = super::fnv(&[text.as_bytes()]);
    let b = super::fnv(&[text.as_bytes(), b"dgf"]);
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xffff,
        b >> 48,
        b & 0xffff_ffff_ffff
    )
}

/// dog-food's test worker: the one a release ships, else built once per
/// version of its source.
fn worker_program() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    if let Some(dll) = shipped(&exe, "worker", "DgfWorker.dll") {
        debug!(
            "worker",
            "the test worker ships with dgf: {}",
            dll.display()
        );
        return Ok(dll);
    }
    built(
        "worker",
        "DgfWorker",
        WORKER_CS,
        WORKER_PROJECT,
        "dog-food's test worker",
    )
}

/// dog-food's worker for Microsoft.Testing.Platform test applications: the
/// one a release ships, else built once per version of its source.
fn mtp_program() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    if let Some(dll) = shipped(&exe, "mtp", "DgfMtp.dll") {
        debug!(
            "worker",
            "the testing platform's worker ships with dgf: {}",
            dll.display()
        );
        return Ok(dll);
    }
    built(
        "mtp",
        "DgfMtp",
        MTP_CS,
        MTP_PROJECT,
        "dog-food's worker for Microsoft.Testing.Platform",
    )
}

/// A worker a release ships beside the binary `exe`, which then needs no
/// build or NuGet feed on this machine; a binary built from source has none.
/// The binary can be run through a link, so the folder it links to counts too.
fn shipped(exe: &Path, folder: &str, file: &str) -> Option<PathBuf> {
    let beside = |dir: &Path| {
        let dll = dir.join("engine").join(folder).join(file);
        dll.is_file().then_some(dll)
    };
    beside(exe.parent()?).or_else(|| beside(std::fs::canonicalize(exe).ok()?.parent()?))
}

/// Builds the C# project `name` (`source` in `assembly`.cs, `project` in
/// `assembly`.csproj) once per version of its text, and returns its dll;
/// other dgf processes wait while one builds it.
fn built(
    name: &str,
    assembly: &str,
    source: &str,
    project: &str,
    what: &str,
) -> Result<PathBuf, String> {
    let version = super::fnv(&[source.as_bytes(), project.as_bytes()]);
    let dir = super::sandbox::temp_root()
        .join("dgf-mutate")
        .join(format!("{name}-{version:016x}"));
    let dll = dir.join("out").join(format!("{assembly}.dll"));
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let lock = std::fs::File::create(dir.with_extension("lock")).map_err(|err| err.to_string())?;
    lock.lock().map_err(|err| err.to_string())?;
    if dll.exists() {
        return Ok(dll);
    }
    let csproj = dir.join(format!("{assembly}.csproj"));
    std::fs::write(dir.join(format!("{assembly}.cs")), source).map_err(|err| err.to_string())?;
    std::fs::write(&csproj, project).map_err(|err| err.to_string())?;
    let out = dir.join("out");
    let build = || {
        dotnet(&[
            "build",
            &csproj.to_string_lossy(),
            "-c",
            "Release",
            "-o",
            &out.to_string_lossy(),
            "-nologo",
            "-v",
            "q",
        ])
        .output()
        .map_err(|err| format!("could not run dotnet: {err}"))
    };
    // A build that fails at once is mostly the machine (a restore that
    // timed out, a build server shutting down): it is tried once more.
    let mut output = build()?;
    if !output.status.success() {
        output = build()?;
    }
    if !output.status.success() {
        return Err(format!(
            "could not build {what}:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(dll)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_earns_workers_by_its_tests_time_up_to_the_jobs() {
        let tests: Vec<String> = (0..1841).map(|i| format!("A.B.T{i}")).collect();
        let none = HashMap::new();
        assert_eq!(baseline_workers(&tests, &none, 3), 3, "92 s by default");
        assert_eq!(
            baseline_workers(&tests[..198], &none, 3),
            1,
            "10 s by default"
        );
        assert_eq!(
            baseline_workers(&tests, &none, 2),
            2,
            "no more than the jobs"
        );
        assert_eq!(baseline_workers(&[], &none, 3), 1);
        let timed: HashMap<String, Duration> = tests[..198]
            .iter()
            .map(|t| (t.clone(), Duration::from_secs(1)))
            .collect();
        assert_eq!(
            baseline_workers(&tests[..198], &timed, 16),
            10,
            "by last time"
        );
    }

    #[test]
    fn slots_let_no_more_than_their_count_run_and_free_up_when_dropped() {
        let slots = Slots::new(2);
        let running = std::sync::atomic::AtomicUsize::new(0);
        let most = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..6 {
                scope.spawn(|| {
                    let _slot = slots.hold();
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    running.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        assert_eq!(most.load(Ordering::SeqCst), 2);
        assert_eq!(*slots.free.lock().unwrap(), 2);
    }

    #[test]
    fn only_compiler_errors_show_the_restore_worked() {
        assert!(compiler_errors(&[
            "Shop/Pricing.cs(12,9): error CS0165: Use of unassigned local variable".into()
        ]));
        assert!(!compiler_errors(&[
            "Shop.csproj : error NU1101: Unable to find package".into()
        ]));
    }

    #[test]
    fn compiler_errors_point_at_a_file_and_line() {
        let line = r"D:\tmp\sandbox\src\Shop\Pricing.cs(22,13): error CS0165: Use of unassigned local variable 'f' [D:\tmp\Shop.csproj]";
        let (path, number) = exec::error_location(line).unwrap();
        assert_eq!(path.file_name().unwrap(), "Pricing.cs");
        assert_eq!(number, 22);
        assert!(exec::error_location("Build succeeded.").is_none());
    }

    #[test]
    fn a_missing_reference_assembly_is_told_apart_from_a_mutant() {
        let line = r"CSC : error CS0006: Metadata file 'D:\s\Scheduling\obj\Debug\net10.0\ref\Scheduling.dll' could not be found [D:\s\Api.csproj]";
        assert!(missing_reference(&[line.to_string()]));
        assert!(!missing_reference(&[
            r"D:\s\A.cs(3,1): error CS0165: Use of unassigned local variable 'f'".to_string()
        ]));
    }

    #[test]
    fn reach_lines_split_tests_and_outside() {
        let mut reach = Reach::default();
        record_reach(
            &mut reach,
            &[vec![
                "A.one\t3".into(),
                "A.two\t3".into(),
                "\t5".into(),
                "junk".into(),
            ]],
            &[],
        );
        assert_eq!(reach.by_mutant[&3].len(), 2);
        assert!(reach.outside.contains(&5));
    }

    #[test]
    fn a_mutant_reached_before_its_test_was_reported_belongs_to_that_tests_class() {
        let mut reach = Reach::default();
        for test in ["Ns.Rules.one", "Ns.Rules.two", "Ns.Other.one"] {
            reach.home.insert(test.into(), "Ns.Tests".into());
        }
        record_reach(
            &mut reach,
            &[vec!["\t4".into(), "\t9\tNs.Other.one".into()]],
            &["Ns.Rules.two\t4".into(), "Ns.Rules.two\t9".into()],
        );
        assert!(!reach.outside.contains(&4));
        assert_eq!(
            reach.by_mutant[&4],
            BTreeSet::from(["Ns.Rules.one".to_string(), "Ns.Rules.two".to_string()])
        );
        assert!(reach.outside.contains(&9), "run once: stays outside");
    }

    #[test]
    fn the_baseline_splits_by_whole_classes_into_even_groups() {
        let tests: Vec<String> = ["A.T.a", "A.T.b", "A.T.c", "A.U.a", "A.V.a(1)", "A.V.a(2)"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        let none = HashMap::new();
        let groups = split_by_class(&tests, 2, &none);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], ["A.T.a", "A.T.b", "A.T.c"]);
        assert_eq!(groups[1].len(), 3);
        assert_eq!(
            split_by_class(&tests, 8, &none).len(),
            3,
            "no more groups than classes"
        );
        assert_eq!(split_by_class(&tests, 1, &none).len(), 1);
        // With last run's times, a single slow test gets a group of its own.
        let times = HashMap::from([("A.U.a".to_string(), Duration::from_secs(20))]);
        let timed = split_by_class(&tests, 2, &times);
        assert!(timed.contains(&vec!["A.U.a".to_string()]), "{timed:?}");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("times.tsv");
        std::fs::write(&path, "ms\tproject\ttest\n20000\tA.Tests\tA.U.a\n").unwrap();
        assert_eq!(read_times(&path), times);
        assert!(splittable(None));
        assert!(splittable(Some("FullyQualifiedName~Pricing|DisplayName=x")));
        assert!(splittable(Some(
            "(FullyQualifiedName!~Slow)&FullyQualifiedName~A"
        )));
        assert!(!splittable(Some("Category=Unit")));
    }

    #[test]
    fn code_run_once_stays_outside_and_code_run_per_test_belongs_to_its_tests() {
        let mut reach = Reach::default();
        let line = |id: usize, test: &str, kind: &str| format!("\t{id}\t{test}\t{kind}");
        record_reach(
            &mut reach,
            &[
                // One process: a static constructor once, a cache per test
                // object twice, and a helper that two classes' static
                // constructors call, each first touched in another test.
                vec![
                    line(7, "A.one", "cctor"),
                    line(8, "A.one", "lazy"),
                    line(8, "A.two", "lazy"),
                    "\t9".into(),
                    line(6, "A.one", "cctor"),
                    line(6, "B.one", "cctor"),
                ],
                // Another process of the same project builds the static again.
                vec![line(7, "A.three", "cctor")],
            ],
            &[],
        );
        assert!(reach.outside.contains(&7) && reach.outside.contains(&9));
        assert!(!reach.outside.contains(&8));
        assert_eq!(reach.by_mutant[&8].len(), 2);
        // Seen in two tests, but in static constructors: once per process.
        assert!(reach.outside.contains(&6));
        assert_eq!(
            reach.outside_in[&6],
            BTreeSet::from(["A.one".to_string(), "B.one".to_string()])
        );
        // A line without a kind (an older switch) is read as a static constructor.
        let mut old = Reach::default();
        record_reach(
            &mut old,
            &[vec!["\t5\tA.one".into(), "\t5\tA.two".into()]],
            &[],
        );
        assert!(old.outside.contains(&5));
    }

    #[test]
    fn a_fresh_run_puts_the_reaching_tests_first_then_the_classes_the_static_ran_in() {
        let mut reach = Reach::default();
        for (test, ms) in [
            ("Ns.A.one", 30),
            ("Ns.A.two", 10),
            ("Ns.B.one", 20),
            ("Ns.C.one", 5),
            ("Ns.C.bad", 1),
        ] {
            reach.home.insert(test.into(), "Ns.Tests".into());
            reach.times.insert(test.into(), Duration::from_millis(ms));
        }
        reach.failing.insert("Ns.C.bad".into());
        reach
            .by_mutant
            .insert(4, BTreeSet::from(["Ns.B.one".to_string()]));
        reach.outside.insert(4);
        reach
            .outside_in
            .insert(4, BTreeSet::from(["Ns.A.one".to_string()]));
        let order = fresh_order(4, &reach);
        assert_eq!(
            order["Ns.Tests"],
            ["Ns.B.one", "Ns.A.two", "Ns.A.one", "Ns.C.one"]
        );
        // Not reached outside any test: only the tests that reach it.
        reach.outside.remove(&4);
        assert_eq!(fresh_order(4, &reach)["Ns.Tests"], ["Ns.B.one"]);
    }

    #[test]
    fn a_worker_shipped_beside_the_binary_needs_no_build() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("dgf");
        std::fs::write(&exe, "").unwrap();
        assert_eq!(shipped(&exe, "worker", "DgfWorker.dll"), None);
        let folder = dir.path().join("engine").join("worker");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("DgfWorker.dll"), "").unwrap();
        assert!(shipped(&exe, "worker", "DgfWorker.dll")
            .unwrap()
            .ends_with("engine/worker/DgfWorker.dll"));
    }

    #[test]
    fn modern_target_frameworks_have_a_version() {
        assert_eq!(version_of("net10.0"), Some((10, 0)));
        assert_eq!(version_of("net8.0-windows"), Some((8, 0)));
        assert_eq!(version_of("netcoreapp3.1"), Some((3, 1)));
        assert_eq!(version_of("net48"), None);
        assert_eq!(version_of("net462"), None);
        assert_eq!(version_of("netstandard2.0"), None);
    }

    #[test]
    fn several_target_frameworks_run_on_the_newest_this_machine_has() {
        let has = |frameworks: &str, runtimes: &[u32]| newest_runnable(frameworks, runtimes);
        assert_eq!(has("net48;net8.0;net10.0", &[8, 10]).unwrap(), "net10.0");
        assert_eq!(has("net8.0;net10.0", &[8]).unwrap(), "net8.0");
        // None installed: the newest, whose worker then says what is missing.
        assert_eq!(has("net8.0;net10.0", &[6]).unwrap(), "net10.0");
        let refused = has("net48;net472", &[10]).unwrap_err();
        assert!(refused.contains("net48;net472"), "{refused}");
    }

    #[test]
    fn the_runtimes_listing_gives_major_versions() {
        let listing = "Microsoft.AspNetCore.App 9.0.1 [C:\\x]\nMicrosoft.NETCore.App 8.0.2 [C:\\x]\nMicrosoft.NETCore.App 11.0.0-rc.2.26504.105 [C:\\x]\n";
        assert_eq!(runtimes_in(listing), [8, 11]);
    }

    #[test]
    fn msbuild_says_what_a_project_builds() {
        let one = parse_properties(
            r#"{"Properties":{"TargetPath":"C:\\t\\A.dll","TargetFramework":"net10.0","TargetFrameworks":"","IsTestingPlatformApplication":"true"}}"#,
        )
        .unwrap();
        assert_eq!(one.path, "C:\\t\\A.dll");
        assert!(one.mtp);
        let several = parse_properties(
            r#"{"Properties":{"TargetPath":"","TargetFramework":"","TargetFrameworks":"net8.0;net10.0","IsTestingPlatformApplication":""}}"#,
        )
        .unwrap();
        assert_eq!(several.frameworks, "net8.0;net10.0");
        assert!(!several.mtp);
        assert!(parse_properties("not json").is_none());
    }

    #[test]
    fn dotnet_framework_is_refused_by_name() {
        let properties = Properties {
            path: "x.dll".into(),
            framework: "net48".into(),
            frameworks: String::new(),
            mtp: false,
        };
        let why = target_of(&properties, None).err().unwrap();
        assert!(why.contains(".NET Framework"), "{why}");
    }

    #[test]
    fn tests_find_the_files_that_declare_their_class() {
        assert_eq!(
            class_of("Shop.Tests.PricingTests.Order_codes(text: \"a.b\")"),
            "PricingTests"
        );
        assert_eq!(class_of("Ns.Outer+Inner.Works"), "Inner");
        assert_eq!(class_of("Ns.Generic`1.Works"), "Generic");
        assert_eq!(
            declared_types("public sealed class PricingTests : IDisposable { record Row(int A); }"),
            ["PricingTests", "Row"]
        );
    }

    #[test]
    fn a_failed_build_is_blamed_on_the_projects_its_errors_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let a = root.join("tests").join("A.Tests").join("A.Tests.csproj");
        let b = root.join("tests").join("B.Tests").join("B.Tests.csproj");
        let lib = root.join("src").join("Lib").join("Lib.csproj");
        let other = root.join("src").join("Win").join("Win.csproj");
        let line = |file: &str, project: &Path, suffix: &str| {
            format!(
                "{file}(3,5): error CS0246: The type 'X' could not be found [{}{suffix}]",
                project.display()
            )
        };
        let tests = [a.clone(), b.clone()];
        let mutated = [lib.clone()];
        // Only B's own errors, one of them from a framework of several, in another case.
        let errors = [
            line("B.cs", &b, "::TargetFramework=net8.0"),
            line(
                "B2.cs",
                &PathBuf::from(b.to_string_lossy().to_uppercase()),
                "",
            ),
        ];
        assert_eq!(
            named_in(&errors, &tests, &mutated),
            Named::Tests(vec![(1, errors[0].clone())])
        );
        // The code under test does not build: no test project is to blame.
        let errors = [line("B.cs", &b, ""), line("L.cs", &lib, "")];
        assert_eq!(named_in(&errors, &tests, &mutated), Named::CodeUnderTest);
        // Another project fails, or an error names none: each test project is built to tell.
        assert_eq!(
            named_in(&[line("W.cs", &other, "")], &tests, &mutated),
            Named::Unknown
        );
        assert_eq!(
            named_in(&["error MSB4025: no project".into()], &tests, &mutated),
            Named::Unknown
        );
    }

    #[test]
    fn a_skipped_test_project_uses_what_it_references_through_others() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |relative: &str, text: &str| {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        };
        write(
            "tests/A.Tests/A.Tests.csproj",
            r#"<Project><ItemGroup>
  <ProjectReference Include="..\..\src\App\App.csproj" />
  <ProjectReference Include="$(Root)\src\Gen\Gen.csproj" />
</ItemGroup></Project>"#,
        );
        write(
            "src/App/App.csproj",
            r#"<Project><ItemGroup><ProjectReference
    Include="../Core/Core.csproj"></ProjectReference></ItemGroup></Project>"#,
        );
        // A cycle does not loop.
        write(
            "src/Core/Core.csproj",
            r#"<Project><ItemGroup><ProjectReference Include="..\App\App.csproj" /></ItemGroup></Project>"#,
        );
        write("src/Other/Other.csproj", "<Project />");
        let used = uses(&root.join("tests/A.Tests/A.Tests.csproj"));
        let key = |relative: &str| project_key(&root.join(relative));
        assert_eq!(
            used,
            HashSet::from([
                key("tests/A.Tests/A.Tests.csproj"),
                key("src/App/App.csproj"),
                key("src/Core/Core.csproj"),
            ])
        );
        assert!(!used.contains(&key("src/Other/Other.csproj")));
    }

    #[test]
    fn missing_restore_output_asks_for_a_restore() {
        let missing = r"C:\sdk\Microsoft.NET.Sdk.targets(308,5): error NETSDK1004: Assets file 'C:\s\obj\project.assets.json' not found. Run a NuGet package restore to generate this file. [C:\s\A.Tests.csproj]";
        assert!(needs_restore(&[missing.to_string()]));
        assert!(!needs_restore(&["A.cs(1,1): error CS0019: no".to_string()]));
    }

    #[test]
    fn a_long_error_is_shortened_on_a_character() {
        assert_eq!(shorten("  short  "), "short");
        let long = "é".repeat(400);
        let short = shorten(&long);
        assert_eq!(short.chars().count(), 303);
        assert!(short.ends_with("..."));
    }

    #[test]
    fn solutions_list_projects_with_stable_guids() {
        let sandbox = Path::new("/s");
        let text = solution(
            sandbox,
            [Path::new("/s/tests/A.Tests/A.Tests.csproj")].into_iter(),
        );
        assert!(
            text.contains(r#"= "A.Tests", "tests\A.Tests\A.Tests.csproj", "{"#),
            "{text}"
        );
        assert_eq!(
            text,
            solution(
                sandbox,
                [Path::new("/s/tests/A.Tests/A.Tests.csproj")].into_iter()
            )
        );
    }
}
