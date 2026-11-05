//! The C# engine: `dotnet build`, and `dotnet test` with dog-food's
//! in-process collector, so every test reports its name to the mutant
//! switches and writes its own result.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use super::exec::{self, fresh_folder, lines_in, write_plan, Located};
use super::projects::TestProject;
use super::sandbox::FileMutants;
use super::worker::{Pool, Reply, Report, Target, Worker};
use super::{
    check_baseline, failing_tests, Budget, BuildError, Engine, Pack, PackRun, Reach, Round,
    SoloRun, TestResults, TestRun,
};
use crate::discover::absolute;

const COLLECTOR_CS: &str = include_str!("../../assets/collector/DgfCollector.cs");
const COLLECTOR_PROJECT: &str = include_str!("../../assets/collector/DgfCollector.csproj");
const WORKER_CS: &str = include_str!("../../assets/worker/DgfWorker.cs");
const WORKER_PROJECT: &str = include_str!("../../assets/worker/DgfWorker.csproj");
const MTP_CS: &str = include_str!("../../assets/mtp/DgfMtp.cs");
const MTP_PROJECT: &str = include_str!("../../assets/mtp/DgfMtp.csproj");
/// Longest a build or the baseline test run may take.
const HOUR: Duration = Duration::from_secs(3600);
/// The subfolder of a run's folder where the collector writes test results.
const RESULTS: &str = "results";
/// Every test's time in a baseline, read by the next run to split its baseline evenly.
const TIMES: &str = "baseline-times.tsv";

#[derive(Clone)]
struct Project {
    path: PathBuf,
    /// The assembly its tests come from, which the collector reports.
    assembly: String,
    /// The user's test filter for this project.
    filter: Option<String>,
}

pub struct Dotnet {
    sandbox: PathBuf,
    projects: Vec<Project>,
    collector: PathBuf,
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
    /// dog-food's test workers, which keep a test project loaded between
    /// runs; `None` when turned off or when the worker could not be built.
    workers: Option<Pool>,
    /// Test project (assembly) → what a worker serves, found after the
    /// first build. A project not here runs with `dotnet test`.
    targets: OnceLock<HashMap<String, Target>>,
    /// Test projects a worker could not serve, and why: they run with
    /// `dotnet test` from then on.
    refused: Mutex<BTreeMap<String, String>>,
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
        worker: bool,
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
        let mutated_solution = sandbox.join("dgf-mutated.sln");
        write_if_changed(
            &mutated_solution,
            &solution(sandbox, mutated.iter().map(PathBuf::as_path)),
        )?;
        let mut notes = Vec::new();
        let workers = match worker.then(worker_program) {
            Some(Ok(program)) => Some(Pool::new(
                program,
                exec::runs_folder(sandbox).join("workers"),
            )),
            Some(Err(err)) => {
                notes.push(format!("dog-food's test worker could not be built, so every test run started `dotnet test`: {err}"));
                None
            }
            None => None,
        };
        Ok(Dotnet {
            sandbox: sandbox.to_path_buf(),
            projects,
            collector: collector()?,
            tests,
            mutated: mutated_solution,
            settings,
            restored: AtomicBool::new(
                std::fs::read_to_string(sandbox.join(RESTORED))
                    .is_ok_and(|t| t == format!("{settings:016x}")),
            ),
            mutated_restored: AtomicBool::new(false),
            build_args,
            workers,
            targets: OnceLock::new(),
            refused: Mutex::new(BTreeMap::new()),
            retired: Mutex::new(HashMap::new()),
            warmup: Mutex::new(HashMap::new()),
            owners: owners(sandbox, files),
            jobs: jobs.max(1),
            last_times: Mutex::new(HashMap::new()),
            notes: Mutex::new(notes),
        })
    }

    /// Runs the given projects (with a test filter each) for `ask` and
    /// collects every test's result; side by side when `parallel`. A project
    /// a worker serves runs there, the others with `dotnet test`.
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
            let ran = match self.target(project) {
                Some(target) => self.in_worker(
                    project,
                    &target,
                    filter.as_deref(),
                    ask,
                    limit,
                    folder,
                    index,
                ),
                None => self.in_command(project, filter.as_deref(), ask, limit, folder, index),
            }?;
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

    /// One project's run with `dotnet test` and dog-food's collector.
    fn in_command(
        &self,
        project: &Project,
        filter: Option<&str>,
        ask: Ask,
        limit: Duration,
        folder: &Path,
        index: usize,
    ) -> Result<(TestRun, Results), String> {
        let mut command = self.test_command(project, filter, folder, index)?;
        match ask {
            Ask::Reach(reached) => command.env("DGF_REACH", reached),
            Ask::Plan(_, plan) => command.env("DGF_PLAN", plan),
            Ask::Fresh(mutant, _) => command.env("DGF_MUTANT", mutant.to_string()),
        };
        let started = Instant::now();
        let status = exec::run(command, limit, &folder.join(exec::log_name(index)))?;
        let mut results = if status == TestRun::TimedOut {
            Results::default()
        } else {
            Results::read(&folder.join(format!("{RESULTS}-{index}")), &self.sandbox)
        };
        if let Ask::Plan(_, plan) = ask {
            // The switches note the planned mutants that took effect next to the plan.
            results.activation_known = true;
            results.activated = activated_in(&plan.with_extension("txt.activated"));
        }
        let status = command_status(status, ask, &project.assembly, &results);
        debug!(
            "dotnet test",
            "{} in {}: {status:?} in {:.1} s (limit {:.1} s), {} tests, their own time {:.1} s",
            project.assembly,
            tag_of(folder),
            started.elapsed().as_secs_f64(),
            limit.as_secs_f64(),
            results.outcome.len(),
            results.times.values().sum::<Duration>().as_secs_f64()
        );
        Ok((status, results))
    }

    /// One project's run in a worker: a warm one from the pool, or a new
    /// one with the mutant on from the start for a mutant reached outside
    /// any test (static setup runs once per process). A worker that timed
    /// out or crashed is stopped; one that cannot start leaves the project
    /// to `dotnet test`.
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
        let pool = self.workers.as_ref().expect("a target needs workers");
        let started = match ask {
            Ask::Fresh(mutant, _) => pool.start(target, &[("DGF_MUTANT", mutant.to_string())]),
            _ => pool.take(target),
        };
        let mut worker = match started {
            Ok(worker) => worker,
            Err(why) => {
                self.refuse(project, why);
                return self.in_command(project, filter, ask, limit, folder, index);
            }
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

    /// Splits the baseline of `project` by test class across workers, one per
    /// share of `--jobs`, started side by side; they stay warm for the mutants.
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
        let pool = self.workers.as_ref().expect("a target needs workers");
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
        let groups = split_by_class(&listed, self.shards(), &times);
        debug!(
            "baseline",
            "{}: {} tests in {} shards of {:?} tests (jobs {}, {} tests timed last run)",
            project.assembly,
            listed.len(),
            groups.len(),
            groups.iter().map(Vec::len).collect::<Vec<_>>(),
            self.jobs,
            times.len()
        );
        if groups.len() < 2 {
            let request = format!("tests\t{}\t{}", filter_file.display(), reached.display());
            let reply = first.request(&request, limit);
            return Ok(self.finish(project, target, first, reply, false, ask, folder, index));
        }
        let mut first = Some(first);
        let ran: Vec<Result<(TestRun, Results, Duration), String>> = std::thread::scope(|scope| {
            let shards: Vec<_> = groups
                .into_iter()
                .enumerate()
                .map(|(shard, names)| {
                    let worker = first.take();
                    scope.spawn(move || {
                        let started = Instant::now();
                        let mut worker = match worker {
                            Some(worker) => worker,
                            None => pool.take(target)?,
                        };
                        let file = folder.join(format!("names-{index}-{shard}.txt"));
                        write_file(&file, &names.join("\n"))?;
                        let request = format!("names\t{}\t{}", file.display(), reached.display());
                        let reply = worker.request(&request, limit);
                        let (status, results) =
                            self.finish(project, target, worker, reply, false, ask, folder, index);
                        Ok((status, results, started.elapsed()))
                    })
                })
                .collect();
            shards
                .into_iter()
                .map(|s| {
                    s.join()
                        .unwrap_or_else(|_| Err("a test worker thread failed".into()))
                })
                .collect()
        });
        let mut status = TestRun::Passed;
        let mut results = Results::default();
        let mut warmup = Duration::ZERO;
        for (shard, ran) in ran.into_iter().enumerate() {
            let (one, shard_results, wall) = ran?;
            status = worst(status, one);
            let tests: Duration = shard_results.times.values().sum();
            debug!(
                "baseline",
                "{} shard {shard}: {one:?} in {:.1} s, its tests {:.1} s",
                project.assembly,
                wall.as_secs_f64(),
                tests.as_secs_f64()
            );
            warmup = warmup.max(wall.saturating_sub(tests));
            results.add(shard_results);
        }
        if let Ok(mut known) = self.warmup.lock() {
            known.insert(project.assembly.clone(), warmup);
        }
        Ok((status, results))
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
            .filter(|p| self.target(p).is_some())
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
        match self.workers.as_ref() {
            Some(pool) if !fresh => pool.give(target, worker),
            _ => worker.stop(),
        }
    }

    /// The worker's report as the collector's results; `planned` runs
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

    /// What a worker serves for `project`, unless it refused the project.
    fn target(&self, project: &Project) -> Option<Target> {
        if self
            .refused
            .lock()
            .is_ok_and(|r| r.contains_key(&project.assembly))
        {
            return None;
        }
        self.targets().get(&project.assembly).cloned()
    }

    /// The test projects a worker serves: each project's built assembly
    /// (MSBuild's TargetPath), once the first build made it. `reach` finds
    /// them before its test runs start; plain threads, not the thread pool
    /// the runs use, so no run waits on itself.
    fn targets(&self) -> &HashMap<String, Target> {
        self.targets.get_or_init(|| {
            if self.workers.is_none() {
                return HashMap::new();
            }
            let found: Vec<(String, Option<Target>)> = std::thread::scope(|scope| {
                let lookups: Vec<_> = self
                    .projects
                    .iter()
                    .map(|p| {
                        scope.spawn(move || {
                            let target = target_path(&p.path)
                                .and_then(|(dll, mtp)| Target::of(&dll, mtp));
                            if let Some(target) = target.as_ref().filter(|t| t.mtp) {
                                xunit_one_at_a_time(&target.dll);
                            }
                            (p.assembly.clone(), target)
                        })
                    })
                    .collect();
                lookups.into_iter().filter_map(|l| l.join().ok()).collect()
            });
            let found = self.with_mtp_worker(found);
            let mut targets = HashMap::new();
            for (assembly, target) in found {
                match target {
                    Some(target) => {
                        debug!(
                            "targets",
                            "{assembly}: {}{}",
                            target.dll.display(),
                            if target.mtp { " (testing platform)" } else { "" }
                        );
                        targets.insert(assembly, target);
                    }
                    None => {
                        debug!("targets", "{assembly}: dotnet test");
                        self.refuse_quietly(&assembly, "it has several target frameworks, no VSTest adapter, or its build output is not where MSBuild says")
                    }
                }
            }
            targets
        })
    }

    /// The runs whose test project can see `mutant`: its build output holds
    /// the mutant's assembly. A test project that does not reference that
    /// assembly cannot notice the mutant, however its tests run. When no
    /// output holds an assembly of that name (another AssemblyName), or a
    /// project runs with `dotnet test`, nothing is left out.
    fn seeing<'a>(
        &self,
        mutant: usize,
        runs: Vec<(&'a Project, Option<String>)>,
    ) -> Vec<(&'a Project, Option<String>)> {
        let Some(owner) = self.owners.get(&mutant) else {
            return runs;
        };
        let sees = |project: &Project| {
            self.target(project).map(|t| {
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

    /// Builds the worker for Microsoft.Testing.Platform test applications
    /// when a test project is one; without it, those projects get no target.
    fn with_mtp_worker(
        &self,
        found: Vec<(String, Option<Target>)>,
    ) -> Vec<(String, Option<Target>)> {
        let wanted = found.iter().any(|(_, t)| t.as_ref().is_some_and(|t| t.mtp));
        let Some(pool) = self.workers.as_ref().filter(|_| wanted) else {
            return found;
        };
        match mtp_program() {
            Ok(program) => {
                pool.set_mtp(program);
                found
            }
            Err(err) => found
                .into_iter()
                .map(|(assembly, target)| match target {
                    Some(t) if t.mtp => {
                        self.refuse_quietly(&assembly, &err);
                        (assembly, None)
                    }
                    other => (assembly, other),
                })
                .collect(),
        }
    }

    /// Workers per test project for the baseline: `--jobs` shared by the projects a worker serves.
    fn shards(&self) -> usize {
        (self.jobs / self.targets().len().max(1)).max(1)
    }

    fn refuse(&self, project: &Project, why: String) {
        self.refuse_quietly(&project.assembly, &why);
    }

    fn refuse_quietly(&self, assembly: &str, why: &str) {
        if let Ok(mut refused) = self.refused.lock() {
            refused
                .entry(assembly.to_string())
                .or_insert_with(|| why.to_string());
        }
    }

    fn warmup_of(&self, assembly: &str) -> Duration {
        self.warmup
            .lock()
            .ok()
            .and_then(|w| w.get(assembly).copied())
            .unwrap_or(Duration::from_secs(30))
    }

    fn test_command(
        &self,
        project: &Project,
        filter: Option<&str>,
        folder: &Path,
        index: usize,
    ) -> Result<Command, String> {
        let settings = folder.join(format!("run-{index}.runsettings"));
        std::fs::write(&settings, run_settings(&self.collector, filter))
            .map_err(|err| format!("{}: {err}", settings.display()))?;
        let results = folder.join(format!("{RESULTS}-{index}"));
        std::fs::create_dir_all(&results).map_err(|err| err.to_string())?;
        let mut command = dotnet(&[
            "test",
            &project.path.to_string_lossy(),
            "--no-build",
            "--nologo",
            "-v",
            "q",
        ]);
        command
            .arg("--settings")
            .arg(&settings)
            .arg("--results-directory")
            .arg(folder.join(format!("trx-{index}")))
            .env("DGF_RESULTS", results);
        Ok(command)
    }

    /// The projects that hold `tests`, each with a filter for its tests;
    /// every project with the user's filter when `tests` is `None`.
    fn runs_for<'a>(
        &'a self,
        tests: Option<&BTreeSet<String>>,
        reach: &Reach,
    ) -> Vec<(&'a Project, Option<String>)> {
        let Some(tests) = tests else {
            return self
                .projects
                .iter()
                .map(|p| (p, p.filter.clone()))
                .collect();
        };
        let mut by_assembly: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for test in tests {
            let home = reach.home.get(test).map_or("", String::as_str);
            by_assembly.entry(home).or_default().push(test);
        }
        self.projects
            .iter()
            .filter_map(|p| {
                by_assembly
                    .get(p.assembly.as_str())
                    .map(|names| (p, Some(filter(names))))
            })
            .collect()
    }

    /// Each test's source files: the one the test adapter reports, else every
    /// file in the test projects that declares the test's class (xUnit does
    /// not report files under `dotnet test`).
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
    /// earlier build of everything left up to date.
    fn build_target(
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
        let Some((passed, text)) =
            exec::output(command, budget.limit(HOUR)).map_err(|e| BuildError::Errors(vec![e]))?
        else {
            return Err(BuildError::OutOfTime);
        };
        if passed {
            return Ok(());
        }
        let found: Vec<String> = text
            .lines()
            .filter(|l| l.contains(": error "))
            .map(str::to_string)
            .collect();
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
        self.targets();
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
        let plan = folder.join("plan.txt");
        write_plan(&plan, &pack.plan)?;
        let tests: BTreeSet<String> = pack.plan.iter().map(|(t, _)| t.clone()).collect();
        let runs = self.runs_for(Some(&tests), reach);
        let grouped = by_project(&pack.plan, reach);
        let (status, results) =
            self.run(&runs, Ask::Plan(&grouped, &plan), limit, &folder, false)?;
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
        let plan = folder.join("plan.txt");
        write_plan(&plan, &entries)?;
        let grouped = by_project(&entries, reach);
        let (status, results) =
            self.run(&runs, Ask::Plan(&grouped, &plan), limit, &folder, false)?;
        Ok((status, results.into_test_results()))
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
        let mut notes = self.notes.lock().map(|n| n.clone()).unwrap_or_default();
        if self.workers.is_some() {
            if let Ok(refused) = self.refused.lock() {
                notes.extend(refused.iter().map(|(project, why)| {
                    format!("{project} ran with `dotnet test` for every test run, not in dog-food's test worker: {why}")
                }));
            }
        }
        notes
    }
}

/// One project's part of a run: its status and results, its assembly, and how long it took.
type ProjectRun = ((TestRun, Results), String, Duration);

/// What a test run is for.
#[derive(Clone, Copy)]
enum Ask<'a> {
    /// No mutant; record which mutants each test reaches in the folder.
    Reach(&'a Path),
    /// Each planned test with its mutant: the plan by test project (for
    /// workers) and as one file (for `dotnet test`).
    Plan(&'a HashMap<String, Vec<(String, usize)>>, &'a Path),
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

/// How a `dotnet test` run ended: it runs every planned test, so a failed
/// run with one of `assembly`'s missing lost its host. (A worker says so
/// itself.)
fn command_status(status: TestRun, ask: Ask, assembly: &str, results: &Results) -> TestRun {
    match ask {
        Ask::Plan(grouped, _)
            if status == TestRun::Failed && planned_missing(grouped.get(assembly), results) =>
        {
            TestRun::Crashed
        }
        _ => status,
    }
}

/// Whether a planned test of this project did not report.
fn planned_missing(planned: Option<&Vec<(String, usize)>>, results: &Results) -> bool {
    planned.is_some_and(|plan| {
        plan.iter()
            .any(|(test, _)| !results.outcome.contains_key(test))
    })
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

/// The tag of the run whose output goes to `folder`.
fn tag_of(folder: &Path) -> String {
    folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
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
    if let Ask::Plan(by_project, _) = ask {
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

/// The assembly a test project builds (MSBuild's TargetPath), and whether it
/// is a Microsoft.Testing.Platform test application; `None` for a project
/// with several target frameworks, which has no TargetPath.
fn target_path(project: &Path) -> Option<(PathBuf, bool)> {
    let output = dotnet(&[
        "msbuild",
        &project.to_string_lossy(),
        "-getProperty:TargetPath",
        "-getProperty:IsTestingPlatformApplication",
        "-nologo",
    ])
    .output()
    .ok()?;
    if !output.status.success() {
        return None;
    }
    target_properties(&String::from_utf8_lossy(&output.stdout))
}

/// MSBuild's `{"Properties": {"TargetPath": ..., "IsTestingPlatformApplication": ...}}`.
fn target_properties(json: &str) -> Option<(PathBuf, bool)> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let properties = &value["Properties"];
    let path = properties["TargetPath"]
        .as_str()
        .filter(|p| !p.is_empty())?;
    let mtp = properties["IsTestingPlatformApplication"]
        .as_str()
        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
    Some((PathBuf::from(path), mtp))
}

/// What the collector wrote: every test's result, its assembly and its file.
#[derive(Default)]
struct Results {
    outcome: HashMap<String, bool>,
    home: HashMap<String, String>,
    /// Test → its source file, relative to the sandbox (and so to the repository).
    files: HashMap<String, String>,
    /// Test → how long it ran (all rows of a theory together).
    times: HashMap<String, Duration>,
    /// Test project (assembly) → how long its `dotnet test` took.
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

    /// Lines "test<TAB>passed|failed<TAB>assembly path<TAB>source file<TAB>ms";
    /// a test that ran several times (theory rows) fails if any row failed.
    fn read(folder: &Path, sandbox: &Path) -> Results {
        let mut results = Results::default();
        let sandbox = absolute(sandbox);
        for line in lines_in(folder) {
            let mut parts = line.split('\t');
            let (Some(test), Some(outcome)) = (parts.next(), parts.next()) else {
                continue;
            };
            let entry = results.outcome.entry(test.to_string()).or_insert(true);
            *entry = *entry && outcome == "passed";
            if let Some(source) = parts.next() {
                results.home.insert(test.to_string(), assembly_name(source));
            }
            let file = parts.next().and_then(|f| relative_file(&sandbox, f));
            if let Some(file) = file {
                results.files.insert(test.to_string(), file);
            }
            if let Some(ms) = parts.next().and_then(|ms| ms.trim().parse().ok()) {
                *results.times.entry(test.to_string()).or_default() += Duration::from_millis(ms);
            }
        }
        results
    }
}

/// Per test project: its `dotnet test` time without its tests' time (host
/// start, discovery, loading).
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

fn assembly_name(source: &str) -> String {
    Path::new(source)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
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

/// `FullyQualifiedName=A|FullyQualifiedName=B`. It goes into the run
/// settings, so its length does not matter.
fn filter(tests: &[&str]) -> String {
    tests
        .iter()
        .map(|t| format!("FullyQualifiedName={}", escape(t)))
        .collect::<Vec<_>>()
        .join("|")
}

fn escape(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '\\' | '(' | ')' | '&' | '|' | '=' | '!' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
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
        let name = assembly_name(&path.to_string_lossy());
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

/// Builds dog-food's collector once per version of its source.
fn collector() -> Result<PathBuf, String> {
    built(
        "collector",
        "DgfCollector",
        COLLECTOR_CS,
        COLLECTOR_PROJECT,
        "dog-food's test collector",
    )
}

/// Builds dog-food's test worker once per version of its source.
fn worker_program() -> Result<PathBuf, String> {
    built(
        "worker",
        "DgfWorker",
        WORKER_CS,
        WORKER_PROJECT,
        "dog-food's test worker",
    )
}

/// Builds dog-food's worker for Microsoft.Testing.Platform test applications
/// once per version of its source.
fn mtp_program() -> Result<PathBuf, String> {
    built(
        "mtp",
        "DgfMtp",
        MTP_CS,
        MTP_PROJECT,
        "dog-food's worker for Microsoft.Testing.Platform",
    )
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

/// Loads the collector, runs tests one at a time inside each test process
/// (so the running test is always known), and applies the test filter.
fn run_settings(collector: &Path, filter: Option<&str>) -> String {
    let filter = filter
        .map(|f| format!("\n    <TestCaseFilter>{}</TestCaseFilter>", xml(f)))
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<RunSettings>
  <RunConfiguration>
    <DisableParallelization>true</DisableParallelization>{filter}
  </RunConfiguration>
  <InProcDataCollectionRunSettings>
    <InProcDataCollectors>
      <InProcDataCollector friendlyName="DgfCollector" uri="InProcDataCollector://dog-food/DgfCollector/1.0" assemblyQualifiedName="DogFood.Collector.DgfCollector, DgfCollector, Version=1.0.0.0, Culture=neutral, PublicKeyToken=null" codebase="{}" />
    </InProcDataCollectors>
  </InProcDataCollectionRunSettings>
  <xUnit>
    <ParallelizeTestCollections>false</ParallelizeTestCollections>
    <MaxParallelThreads>1</MaxParallelThreads>
  </xUnit>
  <NUnit>
    <NumberOfTestWorkers>0</NumberOfTestWorkers>
  </NUnit>
  <MSTest>
    <Parallelize>
      <Workers>1</Workers>
    </Parallelize>
  </MSTest>
</RunSettings>
"#,
        xml(&collector.to_string_lossy())
    )
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn filters_escape_special_characters_and_have_no_length_limit() {
        assert_eq!(
            filter(&["A.B.C", "A.B.D(x)"]),
            "FullyQualifiedName=A.B.C|FullyQualifiedName=A.B.D\\(x\\)"
        );
        let many: Vec<String> = (0..2000)
            .map(|i| format!("Namespace.Class.Test{i}"))
            .collect();
        assert!(filter(&many.iter().map(String::as_str).collect::<Vec<_>>()).len() > 24_000);
    }

    #[test]
    fn run_settings_carry_the_filter_as_xml() {
        let text = run_settings(Path::new("C:/x/DgfCollector.dll"), Some("(A~x)&(B=y|C<z)"));
        assert!(
            text.contains("<TestCaseFilter>(A~x)&amp;(B=y|C&lt;z)</TestCaseFilter>"),
            "{text}"
        );
        assert!(!run_settings(Path::new("x.dll"), None).contains("TestCaseFilter"));
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
    fn msbuild_properties_give_the_test_assembly_and_whether_it_is_a_platform_app() {
        let vstest = r#"{"Properties": {"TargetPath": "C:\\s\\bin\\Debug\\net10.0\\Shop.Tests.dll", "IsTestingPlatformApplication": ""}}"#;
        assert_eq!(
            target_properties(vstest),
            Some((
                PathBuf::from(r"C:\s\bin\Debug\net10.0\Shop.Tests.dll"),
                false
            ))
        );
        let mtp = r#"{"Properties": {"TargetPath": "/s/bin/Shop.Tests.dll", "IsTestingPlatformApplication": "true"}}"#;
        assert_eq!(target_properties(mtp).map(|(_, mtp)| mtp), Some(true));
        // Several target frameworks: no TargetPath.
        assert_eq!(
            target_properties(
                r#"{"Properties": {"TargetPath": "", "IsTestingPlatformApplication": ""}}"#
            ),
            None
        );
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
    fn results_keep_the_worst_outcome_the_assembly_and_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = absolute(dir.path());
        let file = sandbox.join("tests").join("ShopTests.cs");
        std::fs::write(
            sandbox.join("r.txt"),
            format!("T.a\tpassed\tC:/x/Shop.Tests.dll\t{0}\nT.a\tfailed\tC:/x/Shop.Tests.dll\t{0}\nT.b\tpassed\tC:/x/Shop.Tests.dll\t\n", file.display()),
        )
        .unwrap();
        let results = Results::read(&sandbox, &sandbox);
        assert!(!results.outcome["T.a"]);
        assert!(results.outcome["T.b"]);
        assert_eq!(results.home["T.a"], "Shop.Tests");
        assert_eq!(results.files["T.a"], "tests/ShopTests.cs");
        assert!(!results.files.contains_key("T.b"));
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
