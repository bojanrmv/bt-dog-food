//! The TypeScript/JavaScript engine: Vitest, Jest, or Angular's unit-test
//! builder (`ng test`, Vitest inside), run through Node with their JSON
//! reporters. There is no build step; the test runners compile TypeScript as
//! they load it. The mutant switches find the running test
//! through the test framework's `expect` state.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::exec::{self, error_location, fresh_folder, lines_in, write_plan, Located};
use super::sandbox::FileMutants;
use super::warm::{self, Started, Warm, WarmRun};
use super::{
    check_baseline, failing_tests, Budget, BuildError, Engine, Pack, PackRun, Reach, Round,
    SoloRun, TestResults, TestRun,
};
use crate::discover::absolute;

/// Longest the type check or the baseline test run may take.
const HOUR: Duration = Duration::from_secs(3600);

/// The nearest folder above `file` (inside `root`) whose package.json uses
/// Vitest or Jest.
pub fn package_of(file: &Path, root: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(root))
        .find(|dir| runner_of(dir).is_some())
        .map(Path::to_path_buf)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Runner {
    Vitest,
    Jest,
    /// `ng test` of one Angular project (`@angular/build:unit-test`).
    Angular,
}

pub struct NodeTests {
    /// The package folder whose test runner is used, inside the sandbox.
    package: PathBuf,
    runner: Runner,
    /// The runner's script, found in node_modules above the package.
    script: PathBuf,
    /// Angular only: the projects that hold a mutated file; each runs its
    /// own `ng test` with its own test files.
    angular: Vec<AngularProject>,
    sandbox: PathBuf,
    /// The mutated files: compiler errors elsewhere are not the mutants' doing.
    mutated: HashSet<PathBuf>,
    /// Vitest and Angular: the test runners kept alive between runs, per
    /// Angular project (by name), or under "" for the package. The first does
    /// the baseline; the others are spares for a large run.
    warm: HashMap<String, Vec<Mutex<Slot>>>,
    jobs: usize,
}

/// A test runner kept alive between runs, as far as it got.
enum Slot {
    /// Not started; the first run, which runs every test, starts it.
    NotStarted,
    /// Not started; a run that finds the others busy starts it.
    Spare,
    Running(Warm),
    /// It could not serve: each run starts a test runner of its own.
    Cold,
}

/// How many mutants are worth another test runner: it takes as long to start
/// as about ten test runs, and a run needs one for every few mutants.
const MUTANTS_PER_RUNNER: usize = 100;
/// The most runners next to the first, however many jobs there are: each holds
/// a build in memory, and test runs of one test file gain little from more.
const MOST_SPARES: usize = 2;
/// What must be free to start another runner.
const FREE_FOR_A_RUNNER: u64 = 3 * 1024 * 1024 * 1024;
/// How long a spare may take to do its first run.
const SPARE_START: Duration = Duration::from_secs(600);

/// How a slot served a run.
enum Served {
    /// The run is done: how it went, or `None` when it has to start a runner
    /// of its own.
    Done(Option<TestRun>),
    /// This slot cannot serve it.
    Next,
}

/// The most a warm run's limit allows per test file for loading it: the
/// first run's own startup can include a build no later run does.
const WARM_MOST_STARTUP: Duration = Duration::from_secs(3);

impl NodeTests {
    /// Uses the nearest package.json above the mutated files that depends on
    /// Vitest or Jest.
    pub fn prepare(
        sandbox: &Path,
        files: &[FileMutants],
        jobs: usize,
    ) -> Result<NodeTests, String> {
        let first = files.first().ok_or("no TypeScript or JavaScript mutants")?;
        let start = sandbox.join(&first.relative);
        let (package, runner) = start
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(sandbox))
            .find_map(|dir| runner_of(dir).map(|r| (dir.to_path_buf(), r)))
            .ok_or("no package.json with vitest or jest above the mutated files")?;
        let angular = angular_projects(&package, sandbox, files);
        let runner = if angular.is_empty() {
            runner
        } else {
            Runner::Angular
        };
        let script = script_for(&package, runner)
            .ok_or("the test runner is not installed (run npm install in the repository)")?;
        let mutants = files.iter().map(|f| f.mutants.len()).sum();
        let warm = warm_slots(runner, &angular, spares_for(mutants, jobs));
        Ok(NodeTests {
            package,
            runner,
            script,
            angular,
            warm,
            jobs,
            sandbox: sandbox.to_path_buf(),
            mutated: files
                .iter()
                .map(|f| absolute(&sandbox.join(&f.relative)))
                .collect(),
        })
    }

    fn command(
        &self,
        report: &Path,
        files: &[String],
        project: Option<&AngularProject>,
    ) -> Command {
        let mut command = Command::new("node");
        command.current_dir(&self.package).arg(&self.script);
        let report = report.to_string_lossy().into_owned();
        match self.runner {
            Runner::Vitest => command.args([
                "run",
                "--reporter=json",
                &format!("--outputFile={report}"),
                "--coverage.enabled=false",
            ]),
            Runner::Jest => command.args([
                "--json",
                &format!("--outputFile={report}"),
                "--coverage=false",
                "--ci",
            ]),
            Runner::Angular => {
                let project = project.expect("an Angular project");
                command.args([
                    "test",
                    &project.name,
                    "--watch=false",
                    "--no-progress",
                    "--reporters=json",
                    &format!("--output-file={report}"),
                ]);
                command.args(
                    files
                        .iter()
                        .map(|f| format!("--include={}", project.include(f))),
                );
                return command;
            }
        };
        command.args(files);
        command
    }

    /// Runs the given test files (all when empty) and reads every test's
    /// result. In an Angular workspace each project runs its own test files,
    /// one project after the other, within `limit` together.
    /// A run whose packages went missing (another install in the repository
    /// that node_modules links to deletes them first) is tried again after a
    /// wait, twice; then the run fails, instead of every mutant in it
    /// counting as a crash.
    /// The command that starts a test runner to keep alive, with `server` as
    /// its reporter.
    fn warm_command(&self, server: &Path, project: Option<&AngularProject>) -> Command {
        let mut command = Command::new("node");
        command.current_dir(&self.package).arg(&self.script);
        let server = server.to_string_lossy().replace('\\', "/");
        match project {
            Some(project) => command.args([
                "test",
                &project.name,
                "--watch=true",
                "--no-progress",
                &format!("--reporters={server}"),
            ]),
            None => command.args([
                "--watch",
                &format!("--reporter={server}"),
                "--coverage.enabled=false",
            ]),
        };
        command
    }

    /// Does a run in a test runner kept alive for `project`, waiting for a free
    /// one; `None` when there is none and the run has to start one of its own.
    fn warm_run(
        &self,
        project: Option<&AngularProject>,
        run: &WarmRun,
        limit: Duration,
    ) -> Option<TestRun> {
        let name = project.map_or("", |p| p.name.as_str());
        let slots = self.warm.get(name)?;
        loop {
            let mut busy = false;
            for (index, slot) in slots.iter().enumerate() {
                let Ok(mut slot) = slot.try_lock() else {
                    busy = true;
                    continue;
                };
                if let Served::Done(ran) = self.serve(&mut slot, index, project, run, limit) {
                    return ran;
                }
            }
            if !busy {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Does the run in `slot` if it can.
    fn serve(
        &self,
        slot: &mut Slot,
        index: usize,
        project: Option<&AngularProject>,
        run: &WarmRun,
        limit: Duration,
    ) -> Served {
        let name = project.map_or("", |p| p.name.as_str());
        match slot {
            Slot::Cold => Served::Next,
            Slot::NotStarted if !run.files.is_empty() => Served::Done(None),
            Slot::NotStarted => {
                let status = self.start_warm(slot, index, project, run, limit);
                if status.is_none() {
                    *slot = Slot::Cold;
                }
                Served::Done(status)
            }
            Slot::Spare => self.serve_on_spare(slot, index, project, run, limit),
            Slot::Running(_) => Served::Done(run_warm(slot, name, run, limit)),
        }
    }

    /// Starts a spare and does the run in it, if it starts.
    fn serve_on_spare(
        &self,
        slot: &mut Slot,
        index: usize,
        project: Option<&AngularProject>,
        run: &WarmRun,
        limit: Duration,
    ) -> Served {
        if !self.start_spare(slot, index, project) {
            return Served::Next;
        }
        let name = project.map_or("", |p| p.name.as_str());
        Served::Done(run_warm(slot, name, run, limit))
    }

    /// Starts a spare, when the memory for it is free; whether it runs. A run
    /// that cannot get one goes on with the runners there are.
    fn start_spare(&self, slot: &mut Slot, index: usize, project: Option<&AngularProject>) -> bool {
        let name = project.map_or("", |p| p.name.as_str());
        if super::log::free_memory().is_some_and(|free| free < FREE_FOR_A_RUNNER) {
            debug!("warm", "{name}: not enough free memory for another runner");
            *slot = Slot::Cold;
            return false;
        }
        let folder = exec::runs_folder(&self.sandbox).join(format!("spare{index}"));
        let (report, log) = (folder.join("report.json"), folder.join("output.log"));
        let first = WarmRun {
            files: &[],
            env: &[],
            report: &report,
            log: &log,
            folder: &folder,
        };
        if std::fs::create_dir_all(&folder).is_err()
            || self
                .start_warm(slot, index, project, &first, SPARE_START)
                .is_none()
        {
            *slot = Slot::Cold;
            return false;
        }
        true
    }

    /// Starts the test runner to keep alive, which does its first run.
    fn start_warm(
        &self,
        slot: &mut Slot,
        index: usize,
        project: Option<&AngularProject>,
        run: &WarmRun,
        limit: Duration,
    ) -> Option<TestRun> {
        let name = project.map_or("", |p| p.name.as_str());
        let started = Instant::now();
        let tag = name.replace(|c: char| !c.is_alphanumeric(), "-");
        let folder = exec::runs_folder(&self.sandbox).join(format!("warm{tag}-{index}"));
        let server = warm::server_file(&folder)
            .map_err(|err| debug!("warm", "{name}: {err}"))
            .ok()?;
        let command = self.warm_command(&server, project);
        match Warm::start(command, &folder, self.jobs, run, limit) {
            Started::Ready(warm, status) => {
                debug!(
                    "warm",
                    "{name}: built and ran every test in {:.1} s; it stays alive for the mutants",
                    started.elapsed().as_secs_f64()
                );
                *slot = Slot::Running(warm);
                Some(status)
            }
            Started::Refused(reason) => {
                debug!(
                    "warm",
                    "{name}: not kept alive after {:.1} s, each run starts its own: {reason}",
                    started.elapsed().as_secs_f64()
                );
                None
            }
        }
    }

    /// Whether every project's first test runner stayed alive through its first run.
    fn all_warm(&self) -> bool {
        !self.warm.is_empty()
            && self.warm.values().all(|slots| {
                slots
                    .first()
                    .is_some_and(|slot| slot.lock().is_ok_and(|s| matches!(*s, Slot::Running(_))))
            })
    }

    fn run(
        &self,
        files: &[String],
        env: &[(&str, String)],
        limit: Duration,
        folder: &Path,
    ) -> Result<(TestRun, Report), String> {
        for _ in 0..2 {
            let ran = self.run_once(files, env, limit, folder)?;
            if !ran.1.outcome.is_empty() || !packages_missing(folder) {
                return Ok(ran);
            }
            super::progress("the test runner could not load its packages (Cannot find module ... node_modules): waiting 20 s for the install that removed them, then running again");
            std::thread::sleep(PACKAGES_WAIT);
        }
        let ran = self.run_once(files, env, limit, folder)?;
        if ran.1.outcome.is_empty() && packages_missing(folder) {
            return Err(format!(
                "the test runner could not load its packages from {} three times (\"Cannot find module\"): is an install (npm ci, npm install) running in the repository? Run again when it is done",
                self.package.join("node_modules").display()
            ));
        }
        Ok(ran)
    }

    fn run_once(
        &self,
        files: &[String],
        env: &[(&str, String)],
        limit: Duration,
        folder: &Path,
    ) -> Result<(TestRun, Report), String> {
        let started = Instant::now();
        let mut status = TestRun::Passed;
        let mut merged = Report::default();
        for (n, (project, files)) in self.runs(files).into_iter().enumerate() {
            let report = folder.join(format!("report-{n}.json"));
            let log = folder.join(exec::log_name(n));
            let left = limit.saturating_sub(started.elapsed());
            let warm = self.warm_run(
                project,
                &WarmRun {
                    files: &files,
                    env,
                    report: &report,
                    log: &log,
                    folder,
                },
                left,
            );
            let ran = match warm {
                Some(ran) => ran,
                None => {
                    let mut command = self.command(&report, &files, project);
                    command.envs(env.iter().map(|(k, v)| (*k, v.as_str())));
                    let left = limit.saturating_sub(started.elapsed());
                    exec::run(command, left, &log)?
                }
            };
            status = worse(status, ran);
            merged.merge(Report::read(&report, &self.package));
            if status == TestRun::TimedOut {
                break;
            }
        }
        Ok((status, merged))
    }

    /// The runs a set of test files needs: one, or with Angular one per
    /// project, each with the files it holds (every project when `files` is
    /// empty, which runs all their tests).
    fn runs(&self, files: &[String]) -> Vec<(Option<&AngularProject>, Vec<String>)> {
        if self.angular.is_empty() {
            return vec![(None, files.to_vec())];
        }
        self.angular
            .iter()
            .filter_map(|project| {
                let own: Vec<String> = files
                    .iter()
                    .filter(|f| self.owner(f).is_some_and(|o| o.name == project.name))
                    .cloned()
                    .collect();
                (files.is_empty() || !own.is_empty()).then_some((Some(project), own))
            })
            .collect()
    }

    /// What the run without mutants, which took `wall`, says about reach.
    fn reach_of(
        &self,
        report: Report,
        flaky: BTreeSet<String>,
        wall: Duration,
        reached: &Path,
    ) -> Reach {
        // Each name is stored twice (Vitest's and Jest's spelling).
        let tests: Duration = report.times.values().sum::<Duration>() / 2;
        let mut per_file = wall.saturating_sub(tests);
        if self.all_warm() {
            per_file = per_file.min(WARM_MOST_STARTUP);
        }
        let startup = report
            .home
            .values()
            .map(|file| (file.clone(), per_file))
            .collect();
        let package = self
            .package
            .strip_prefix(&self.sandbox)
            .unwrap_or(Path::new(""));
        let mut reach = Reach {
            failing: failing_tests(&report.outcome),
            flaky,
            queuing: report.queuing.iter().cloned().collect(),
            files: report
                .home
                .iter()
                .map(|(test, file)| {
                    (
                        test.clone(),
                        vec![package.join(file).to_string_lossy().replace('\\', "/")],
                    )
                })
                .collect(),
            home: report.home,
            times: report.times,
            startup,
            duration: wall,
            ..Reach::default()
        };
        for line in lines_in(reached) {
            add_reach(&mut reach, &line, &self.package);
        }
        reach
    }

    /// Runs the test files of the tests that failed with no mutant once more,
    /// still recording reach: tests that pass now count as passing (a test
    /// that fails only now and then would otherwise take every mutant only it
    /// reaches with it). Returns those tests.
    fn run_failing_again(
        &self,
        report: &mut Report,
        env: &[(&str, String)],
        budget: Budget,
        folder: &Path,
    ) -> BTreeSet<String> {
        let failing = failing_tests(&report.outcome);
        if failing.is_empty() {
            return BTreeSet::new();
        }
        let again = folder.join("again");
        let files = self.files_for(
            &failing,
            &Reach {
                home: report.home.clone(),
                ..Reach::default()
            },
        );
        let ran = std::fs::create_dir_all(&again)
            .map_err(|err| err.to_string())
            .and_then(|()| self.run(&files, env, budget.limit(HOUR), &again));
        let (status, second) = match ran {
            Ok(ran) => ran,
            Err(err) => {
                debug!("reach", "the failing tests could not run again: {err}");
                return BTreeSet::new();
            }
        };
        let flaky: BTreeSet<String> = failing
            .into_iter()
            .filter(|test| second.outcome.get(test) == Some(&true))
            .collect();
        debug!(
            "reach",
            "{} test names failed with no mutant; run again ({} test files, {status:?}), {} of them passed",
            failing_tests(&report.outcome).len(),
            files.len(),
            flaky.len()
        );
        for test in &flaky {
            report.outcome.insert(test.clone(), true);
        }
        flaky
    }

    /// The chosen Angular project a file (relative to the package) is in.
    fn owner(&self, file: &str) -> Option<&AngularProject> {
        let file = file.replace('\\', "/");
        self.angular
            .iter()
            .filter(|p| p.root.is_empty() || file.starts_with(&format!("{}/", p.root)))
            .max_by_key(|p| p.root.len())
    }

    /// Type-checks one Angular project's tests (its tsconfig.spec.json): the
    /// errors in mutated files.
    fn type_check(
        &self,
        project: &AngularProject,
        budget: Budget,
    ) -> Result<Vec<String>, BuildError> {
        let config = self.package.join(&project.root).join("tsconfig.spec.json");
        let tsc = script_at(&self.package, "node_modules/typescript/bin/tsc");
        let (true, Some(tsc)) = (config.exists(), tsc) else {
            return Ok(Vec::new());
        };
        let mut command = Command::new("node");
        command
            .current_dir(&self.package)
            .arg(tsc)
            .args(["--noEmit", "--pretty", "false", "-p"])
            .arg(&config);
        let Some((_, text)) = exec::output(command, budget.limit(HOUR))
            .map_err(|err| BuildError::Errors(vec![format!("could not run tsc: {err}")]))?
        else {
            return Err(BuildError::OutOfTime);
        };
        let errors: Vec<String> = text
            .lines()
            .filter(|line| {
                error_location(line).is_some_and(|(path, _)| {
                    self.mutated.contains(&absolute(&self.package.join(path)))
                })
            })
            .map(str::to_string)
            .collect();
        Ok(errors)
    }

    /// The test files a mutant's solo run needs: those of the tests that
    /// reach it, and those that load it outside a test. Empty, which runs
    /// every test, when it is reached outside a test in files not known.
    fn solo_files(&self, mutant: usize, reach: &Reach) -> Vec<String> {
        let loaded = reach.outside_files.get(&mutant);
        if reach.outside.contains(&mutant) && loaded.is_none() {
            return Vec::new();
        }
        let mut files: BTreeSet<String> = reach
            .by_mutant
            .get(&mutant)
            .map(|tests| self.files_for(tests, reach).into_iter().collect())
            .unwrap_or_default();
        files.extend(
            loaded
                .into_iter()
                .flatten()
                .map(|f| pattern(f, self.runner)),
        );
        files.into_iter().collect()
    }

    /// What a mutant reached while modules load comes to. Every test of a file
    /// runs with it, so a failure there is not its doing by itself: nobody having
    /// noticed, every test runs (`widen`), and a kill among all of them has to
    /// hold in the files that failed, which can fail by their order in one
    /// process. Then a test that fails with no mutant too (it depends on the
    /// clock, say) killed nothing.
    #[allow(clippy::too_many_arguments)]
    fn settle_outside(
        &self,
        widen: bool,
        env: &[(&str, String)],
        limit: Duration,
        folder: &Path,
        reach: &Reach,
        status: TestRun,
        report: Report,
    ) -> Result<(TestRun, Report), String> {
        let (mut status, mut report) = (status, report);
        if widen {
            (status, report) = self.run(&[], env, limit, folder)?;
            let failed = self.files_that_failed(&report, reach);
            if !failed.is_empty() {
                (status, report) = self.run(&failed, env, limit, folder)?;
            }
        }
        let failed = self.files_that_failed(&report, reach);
        if failed.is_empty() {
            return Ok((status, report));
        }
        let (_, clean) = self.run(&failed, &[], limit, folder)?;
        Ok(without_what_fails_anyway(status, report, &clean))
    }

    /// The test files with a test that passes without mutants and failed here.
    fn files_that_failed(&self, report: &Report, reach: &Reach) -> Vec<String> {
        let failed: BTreeSet<String> = report
            .outcome
            .iter()
            .filter(|(test, passed)| !**passed && !reach.failing.contains(*test))
            .map(|(test, _)| test.clone())
            .collect();
        let homes = Reach {
            home: report.home.clone(),
            ..Reach::default()
        };
        self.files_for(&failed, &homes)
    }

    /// Test files (relative to the package) holding `tests`.
    fn files_for(&self, tests: &BTreeSet<String>, reach: &Reach) -> Vec<String> {
        let files: BTreeSet<String> = tests
            .iter()
            .filter_map(|t| reach.home.get(t).cloned())
            .collect();
        files
            .into_iter()
            .map(|f| pattern(&f, self.runner))
            .collect()
    }
}

/// The Angular projects holding `files`, each once.
fn angular_projects(package: &Path, sandbox: &Path, files: &[FileMutants]) -> Vec<AngularProject> {
    let mut angular: Vec<AngularProject> = Vec::new();
    for project in files
        .iter()
        .filter_map(|file| angular_project(package, &sandbox.join(&file.relative)))
    {
        if !angular.iter().any(|p| p.name == project.name) {
            angular.push(project);
        }
    }
    angular
}

/// How many spare runners a run of `mutants` is worth, at `jobs` at a time.
fn spares_for(mutants: usize, jobs: usize) -> usize {
    (mutants / MUTANTS_PER_RUNNER)
        .min(jobs.saturating_sub(1))
        .min(MOST_SPARES)
}

/// Places for the test runners to keep alive; Jest has none.
fn warm_slots(
    runner: Runner,
    angular: &[AngularProject],
    spares: usize,
) -> HashMap<String, Vec<Mutex<Slot>>> {
    let slots = || {
        std::iter::once(Slot::NotStarted)
            .chain((0..spares).map(|_| Slot::Spare))
            .map(Mutex::new)
            .collect::<Vec<_>>()
    };
    match runner {
        Runner::Jest => HashMap::new(),
        Runner::Vitest => HashMap::from([(String::new(), slots())]),
        Runner::Angular => angular.iter().map(|p| (p.name.clone(), slots())).collect(),
    }
}

/// Does a run in the runner in `slot`; `None` when it has gone, and the run
/// has to start one of its own.
fn run_warm(slot: &mut Slot, name: &str, run: &WarmRun, limit: Duration) -> Option<TestRun> {
    let Slot::Running(warm) = slot else {
        return None;
    };
    let started = Instant::now();
    let ran = warm.run(run, limit);
    debug!(
        "warm",
        "{name}: {} test files in {:.1} s: {ran:?}",
        run.files.len(),
        started.elapsed().as_secs_f64()
    );
    let stuck = warm.stuck;
    match ran {
        Ok(status) => {
            if stuck {
                *slot = Slot::Cold;
            }
            Some(status)
        }
        Err(reason) => {
            debug!("warm", "{name}: no longer kept alive: {reason}");
            *slot = Slot::Cold;
            None
        }
    }
}

/// An error when no test ran because a test file failed to load.
fn nothing_ran(report: &Report, folder: &Path) -> Result<(), String> {
    match &report.first_failure {
        Some(failure) if report.outcome.is_empty() => Err(format!(
            "no test ran without mutants; the first test file that failed: {failure} (all results: {})",
            folder.join("report.json").display()
        )),
        _ => Ok(()),
    }
}

/// `report` with the tests that also fail in `clean`, a run with no mutant,
/// counted as passed: they say nothing about the mutant.
fn without_what_fails_anyway(
    status: TestRun,
    mut report: Report,
    clean: &Report,
) -> (TestRun, Report) {
    for (test, passed) in &clean.outcome {
        if !passed {
            report.outcome.insert(test.clone(), true);
        }
    }
    let failed = report.outcome.values().any(|passed| !passed);
    let status = if status == TestRun::Failed && !failed {
        TestRun::Passed
    } else {
        status
    };
    (status, report)
}

/// Whether a mutant reached while modules load, which the test files that
/// loaded it did not notice, has to run with every test. Where test files share
/// a process, a module loads once, under the first test file that needs it, so
/// the files that loaded it are not all the files that use it.
fn needs_every_test(mutant: usize, files: &[String], report: &Report, reach: &Reach) -> bool {
    let noticed = report
        .outcome
        .iter()
        .any(|(test, passed)| !*passed && !reach.failing.contains(test));
    reach.outside.contains(&mutant) && !files.is_empty() && !noticed
}

/// A test's name as the frameworks spell it: "describe > test" (Vitest) and
/// "describe test" (Jest).
fn names_of(test: &JsonTest) -> [String; 2] {
    let mut parts = test.ancestors.clone();
    parts.push(test.title.clone());
    [parts.join(" > "), parts.join(" ")]
}

/// The words that set only a mock's next value.
const QUEUES: [&str; 2] = ["ValueOnce(", "ImplementationOnce("];

/// The words that start a test, or code that runs for every test.
const TEST_STARTS: [&str; 2] = ["it(", "test("];
const SHARED_STARTS: [&str; 2] = ["beforeEach(", "beforeAll("];

/// Which tests of a test file queue a mock's next value: `None` when the file
/// queues none; inside it, `None` when any test may, because a hook, a helper
/// above the tests or a generated title does it.
fn queuing_titles(text: &str) -> Option<Option<HashSet<&str>>> {
    let queues_in = |part: &str| QUEUES.iter().any(|q| part.contains(q));
    if !queues_in(text) {
        return None;
    }
    let starts = block_starts(text);
    let first = starts.first().map_or(text.len(), |(at, _)| *at);
    if queues_in(&text[..first]) {
        return Some(None);
    }
    let mut titles = HashSet::new();
    for (index, (at, shared)) in starts.iter().enumerate() {
        let end = starts.get(index + 1).map_or(text.len(), |(next, _)| *next);
        let block = &text[*at..end];
        if !queues_in(block) {
            continue;
        }
        match (shared, title_of(block)) {
            (false, Some(title)) => titles.insert(title),
            _ => return Some(None),
        };
    }
    Some(Some(titles))
}

/// Where each test or hook starts, and whether it is a hook.
fn block_starts(text: &str) -> Vec<(usize, bool)> {
    let mut starts: Vec<(usize, bool)> = Vec::new();
    for (words, shared) in [(TEST_STARTS, false), (SHARED_STARTS, true)] {
        for word in words {
            starts.extend(
                text.match_indices(word)
                    .filter(|(at, _)| starts_word(text, *at))
                    .map(|(at, _)| (at, shared)),
            );
        }
    }
    starts.sort_unstable();
    starts
}

fn starts_word(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '.')
}

/// The title of a test that starts `block`, when it is one plain string.
fn title_of(block: &str) -> Option<&str> {
    let after = block[block.find('(')? + 1..].trim_start();
    let quote = after.chars().next().filter(|c| "'\"`".contains(*c))?;
    let rest = &after[1..];
    let title = &rest[..rest.find(quote)?];
    (!title.contains('\\') && !title.contains("${")).then_some(title)
}

/// What a pack's run shows. A test that failed although its mutant never took
/// effect in it failed by what an earlier test left behind (a mock's queued
/// value, say), and a pass there says nothing: the shared logic gives both
/// mutants a run alone.
fn pack_results(
    outcome: HashMap<String, bool>,
    plan: &[(String, usize)],
    took_effect: &str,
) -> TestResults {
    let activated: BTreeSet<(String, usize)> = took_effect
        .lines()
        .filter_map(|line| {
            let (test, id) = line.rsplit_once('\t')?;
            Some((test.to_string(), id.trim().parse().ok()?))
        })
        .collect();
    let polluted = plan
        .iter()
        .filter(|pair| outcome.get(&pair.0) == Some(&false) && !activated.contains(*pair))
        .map(|(test, _)| test.clone())
        .collect();
    TestResults {
        polluted,
        activated: Some(activated),
        ..TestResults::of(outcome)
    }
}

impl Engine for NodeTests {
    fn confirms_survivors(&self) -> bool {
        true
    }

    /// Vitest and Jest strip types without checking them, but Angular's
    /// builder type-checks, and a mutant can break narrowing (`typeof a ===
    /// "number" && ...` turned into `||`): tsc finds those first.
    fn build(&self, _: Round, budget: Budget) -> Result<(), BuildError> {
        let mut errors = Vec::new();
        for project in &self.angular {
            errors.extend(self.type_check(project, budget)?);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(BuildError::Errors(errors))
        }
    }

    fn locate(&self, files: &[FileMutants], errors: &[String]) -> Located {
        exec::locate(&self.sandbox, &self.package, files, errors)
    }

    fn reach(&self, budget: Budget) -> Result<Option<Reach>, String> {
        let folder = fresh_folder(&self.sandbox, "baseline")?;
        let reached = folder.join("reach");
        std::fs::create_dir_all(&reached).map_err(|err| err.to_string())?;
        let started = Instant::now();
        let env = [("DGF_REACH", reached.to_string_lossy().into_owned())];
        let (status, mut report) = self.run(&[], &env, budget.limit(HOUR), &folder)?;
        if status == TestRun::TimedOut && budget.spent() {
            return Ok(None);
        }
        nothing_ran(&report, &folder)?;
        let flaky = self.run_failing_again(&mut report, &env, budget, &folder);
        check_baseline(status, &report.outcome, Some(&folder))?;
        Ok(Some(self.reach_of(
            report,
            flaky,
            started.elapsed(),
            &reached,
        )))
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
        let activated = folder.join("activated.txt");
        let tests: BTreeSet<String> = pack.plan.iter().map(|(t, _)| t.clone()).collect();
        let files = self.files_for(&tests, reach);
        let (status, report) = self.run(
            &files,
            &[
                ("DGF_PLAN", plan.to_string_lossy().into_owned()),
                ("DGF_ACTIVATED", activated.to_string_lossy().into_owned()),
            ],
            limit,
            &folder,
        )?;
        let took_effect = std::fs::read_to_string(&activated).unwrap_or_default();
        Ok((status != TestRun::TimedOut)
            .then(|| pack_results(report.outcome, &pack.plan, &took_effect)))
    }

    fn solo(
        &self,
        mutant: usize,
        reach: &Reach,
        _fresh: bool,
        limit: Duration,
        tag: &str,
    ) -> Result<SoloRun, String> {
        let files = self.solo_files(mutant, reach);
        let folder = fresh_folder(&self.sandbox, tag)?;
        let env = [("DGF_MUTANT", mutant.to_string())];
        let (mut status, mut report) = self.run(&files, &env, limit, &folder)?;
        if reach.outside.contains(&mutant) {
            let widen = needs_every_test(mutant, &files, &report, reach);
            (status, report) =
                self.settle_outside(widen, &env, limit, &folder, reach, status, report)?;
        }
        // The runner runs every test of the files, so a failed run with a
        // reaching test missing lost its worker.
        let missing = reach
            .by_mutant
            .get(&mutant)
            .is_some_and(|tests| tests.iter().any(|t| !report.outcome.contains_key(t)));
        let status = if status == TestRun::Failed && missing {
            TestRun::Crashed
        } else {
            status
        };
        Ok((status, TestResults::of(report.outcome)))
    }

    fn test_projects(&self) -> Vec<String> {
        let runners: Vec<String> = match self.runner {
            Runner::Angular => self
                .angular
                .iter()
                .map(|p| format!("ng test {}", p.name))
                .collect(),
            Runner::Jest => vec!["jest".into()],
            Runner::Vitest => vec!["vitest".into()],
        };
        runners
            .into_iter()
            .map(|runner| format!("{} ({runner})", self.package.display()))
            .collect()
    }

    fn crash_reason(&self, tag: &str) -> Option<String> {
        exec::crash_reason(&exec::runs_folder(&self.sandbox).join(tag))
    }
}

/// How long a run waits for packages an install removed.
const PACKAGES_WAIT: Duration = Duration::from_secs(20);

/// Whether a run's output in `folder` says a package could not be loaded
/// from node_modules: the machine, not the mutant.
fn packages_missing(folder: &Path) -> bool {
    exec::crash_reason(folder)
        .is_some_and(|said| said.contains("Cannot find module") && said.contains("node_modules"))
}

/// The worse of two runs' ends: a timeout, then a failure.
fn worse(a: TestRun, b: TestRun) -> TestRun {
    let rank = |r: TestRun| match r {
        TestRun::Passed => 0,
        TestRun::Failed => 1,
        TestRun::Crashed => 2,
        TestRun::TimedOut => 3,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// One reach record, `test<TAB>id<TAB>spec file`. A mutant reached with no
/// test running, or under the name of a test that lives in another file
/// (a name left over from that file, while this one loads its modules),
/// was reached outside any test: by loading `spec file`.
fn add_reach(reach: &mut Reach, line: &str, package: &Path) {
    let (rest, file) = match line.trim_end().rsplit_once('\t') {
        Some((rest, file)) if rest.contains('\t') => (rest, file),
        _ => (line.trim_end(), ""),
    };
    let Some((test, id)) = rest.rsplit_once('\t') else {
        return;
    };
    let Ok(id) = id.trim().parse::<usize>() else {
        return;
    };
    let file = Path::new(file)
        .strip_prefix(package)
        .map_or(file.to_string(), |p| p.to_string_lossy().into_owned())
        .replace('\\', "/");
    let stale = !file.is_empty()
        && reach
            .home
            .get(test)
            .is_some_and(|home| home.replace('\\', "/") != file);
    if test.is_empty() || stale {
        reach.outside.insert(id);
        if !file.is_empty() {
            reach.outside_files.entry(id).or_default().insert(file);
        }
    } else {
        reach
            .by_mutant
            .entry(id)
            .or_default()
            .insert(test.to_string());
    }
}

/// A test file as a command-line filter: Vitest matches parts of paths, Jest
/// takes a regular expression.
fn pattern(file: &str, runner: Runner) -> String {
    let file = file.replace('\\', "/");
    match runner {
        Runner::Vitest | Runner::Angular => file,
        Runner::Jest => file
            .chars()
            .flat_map(|c| {
                if ".+*?^$()[]{}|".contains(c) {
                    vec!['\\', c]
                } else {
                    vec![c]
                }
            })
            .collect(),
    }
}

#[derive(Deserialize)]
struct PackageJson {
    #[serde(default, rename = "dependencies")]
    deps: HashMap<String, serde_json::Value>,
    #[serde(default, rename = "devDependencies")]
    dev: HashMap<String, serde_json::Value>,
}

fn runner_of(dir: &Path) -> Option<Runner> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let package: PackageJson = serde_json::from_str(&text).ok()?;
    let has = |name: &str| package.deps.contains_key(name) || package.dev.contains_key(name);
    if has("vitest") {
        Some(Runner::Vitest)
    } else {
        has("jest").then_some(Runner::Jest)
    }
}

/// An Angular project from angular.json; paths are relative to the package.
#[derive(Debug, PartialEq, Eq)]
struct AngularProject {
    name: String,
    root: String,
    /// Where `ng test` resolves `--include` (`<root>/src` when not set).
    source_root: String,
}

impl AngularProject {
    /// A test file (relative to the package) as `--include` takes it:
    /// relative to the source root, with `..` for files beside it (a
    /// library's secondary entry points sit next to its `src`).
    fn include(&self, file: &str) -> String {
        let file: Vec<&str> = file.split('/').filter(|p| !p.is_empty()).collect();
        let base: Vec<&str> = self
            .source_root
            .split('/')
            .filter(|p| !p.is_empty())
            .collect();
        let shared = file.iter().zip(&base).take_while(|(a, b)| a == b).count();
        let up = std::iter::repeat_n("..", base.len() - shared);
        up.chain(file[shared..].iter().copied())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// The Angular project holding `file` whose `test` target is Angular's
/// unit-test builder, from the package's angular.json.
fn angular_project(package: &Path, file: &Path) -> Option<AngularProject> {
    let text = std::fs::read_to_string(package.join("angular.json")).ok()?;
    let workspace: serde_json::Value = serde_json::from_str(&text).ok()?;
    let relative = file
        .strip_prefix(package)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    workspace["projects"]
        .as_object()?
        .iter()
        .filter(|(_, p)| p["architect"]["test"]["builder"] == "@angular/build:unit-test")
        .map(|(name, p)| {
            let root = p["root"].as_str().unwrap_or("").trim_end_matches('/');
            let source_root = match p["sourceRoot"].as_str() {
                Some(dir) => dir.trim_end_matches('/').to_string(),
                None if root.is_empty() => "src".to_string(),
                None => format!("{root}/src"),
            };
            AngularProject {
                name: name.clone(),
                root: root.to_string(),
                source_root,
            }
        })
        .filter(|p| p.root.is_empty() || relative.starts_with(&format!("{}/", p.root)))
        .max_by_key(|p| p.root.len())
}

/// The runner's entry script in node_modules, looking upwards from the package.
fn script_for(package: &Path, runner: Runner) -> Option<PathBuf> {
    let relative = match runner {
        Runner::Vitest => "node_modules/vitest/vitest.mjs",
        Runner::Jest => "node_modules/jest/bin/jest.js",
        Runner::Angular => "node_modules/@angular/cli/bin/ng.js",
    };
    script_at(package, relative)
}

/// `relative` in the nearest folder above `package` that has it.
fn script_at(package: &Path, relative: &str) -> Option<PathBuf> {
    package
        .ancestors()
        .map(|dir| dir.join(relative))
        .find(|path| path.exists())
}

/// The JSON report both Vitest and Jest write (`--reporter=json`, `--json`).
#[derive(Default)]
struct Report {
    /// Test name → passed. Each test is stored under the names the
    /// frameworks use for the running test: "describe > test" (Vitest) and
    /// "describe test" (Jest).
    outcome: HashMap<String, bool>,
    /// Test name → its file, relative to the package.
    home: HashMap<String, String>,
    /// Test name → how long it ran.
    times: HashMap<String, Duration>,
    /// The first test file that failed with no test reported, and why.
    first_failure: Option<String>,
    /// Tests that queue a mock's next value: one that fails can leave the value
    /// for the next test that calls the mock.
    queuing: HashSet<String>,
}

#[derive(Deserialize)]
struct JsonReport {
    #[serde(default, rename = "testResults")]
    files: Vec<JsonFile>,
}

#[derive(Deserialize)]
struct JsonFile {
    name: String,
    #[serde(default, rename = "assertionResults")]
    tests: Vec<JsonTest>,
    /// Why the file failed before its tests ran (an import, a setup file).
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct JsonTest {
    #[serde(default, rename = "ancestorTitles")]
    ancestors: Vec<String>,
    title: String,
    status: String,
    /// Milliseconds.
    #[serde(default)]
    duration: Option<f64>,
}

impl Report {
    /// Another run's results added to these.
    fn merge(&mut self, other: Report) {
        self.outcome.extend(other.outcome);
        self.home.extend(other.home);
        self.times.extend(other.times);
        self.queuing.extend(other.queuing);
        if self.first_failure.is_none() {
            self.first_failure = other.first_failure;
        }
    }

    fn read(path: &Path, package: &Path) -> Report {
        let parsed: Option<JsonReport> = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok());
        let mut report = Report::default();
        for file in parsed.map(|p| p.files).unwrap_or_default() {
            let home = Path::new(&file.name)
                .strip_prefix(package)
                .map_or(file.name.clone(), |p| p.to_string_lossy().into_owned());
            if file.tests.is_empty() && !file.message.trim().is_empty() {
                report.first_failure.get_or_insert_with(|| {
                    let first = file.message.lines().next().unwrap_or_default().trim();
                    format!("{home}: {first}")
                });
            }
            report.note_queuing(&file);
            for test in file.tests {
                report.add(&test, &home);
            }
        }
        report
    }

    /// Notes the tests of `file` that queue a mock's next value.
    fn note_queuing(&mut self, file: &JsonFile) {
        let Ok(text) = std::fs::read_to_string(&file.name) else {
            return;
        };
        let Some(queued) = queuing_titles(&text) else {
            return;
        };
        for test in &file.tests {
            if queued
                .as_ref()
                .is_none_or(|titles| titles.contains(test.title.as_str()))
            {
                self.queuing.extend(names_of(test));
            }
        }
    }

    fn add(&mut self, test: &JsonTest, home: &str) {
        let passed = test.status != "failed";
        let time = Duration::from_secs_f64(test.duration.unwrap_or(0.0).max(0.0) / 1000.0);
        for name in names_of(test) {
            let entry = self.outcome.entry(name.clone()).or_insert(true);
            *entry = *entry && passed;
            *self.times.entry(name.clone()).or_default() += time;
            self.home.insert(name, home.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_that_also_fails_with_no_mutant_killed_nothing() {
        let report = |rows: &[(&str, bool)]| Report {
            outcome: rows.iter().map(|(t, p)| (t.to_string(), *p)).collect(),
            ..Report::default()
        };
        let (status, kept) = without_what_fails_anyway(
            TestRun::Failed,
            report(&[("clock", false), ("fine", true)]),
            &report(&[("clock", false), ("fine", true)]),
        );
        assert_eq!(
            status,
            TestRun::Passed,
            "the only failure is not the mutant's"
        );
        assert!(kept.outcome["clock"]);
        let (status, kept) = without_what_fails_anyway(
            TestRun::Failed,
            report(&[("clock", false), ("real", false)]),
            &report(&[("clock", false), ("real", true)]),
        );
        assert_eq!(status, TestRun::Failed, "this one is");
        assert!(!kept.outcome["real"]);
        let (status, _) = without_what_fails_anyway(TestRun::TimedOut, report(&[]), &report(&[]));
        assert_eq!(status, TestRun::TimedOut);
    }

    #[test]
    fn a_large_run_is_worth_more_test_runners_a_small_one_is_not() {
        assert_eq!(spares_for(55, 3), 0, "a branch's mutants");
        assert_eq!(spares_for(150, 3), 1);
        assert_eq!(spares_for(326, 3), 2);
        assert_eq!(spares_for(326, 2), 1, "no more than the jobs");
        assert_eq!(spares_for(326, 1), 0);
        assert_eq!(spares_for(5000, 16), 2, "each holds a build in memory");
    }

    #[test]
    fn tests_that_queue_a_mocks_next_value_are_found() {
        assert_eq!(
            queuing_titles("it('a', () => { m.mockReturnValue(1); });"),
            None
        );
        let some = "describe('s', () => {\n  it('a', () => { m.mockRejectedValueOnce(1); });\n  it('b', () => { m(); });\n  test(\"c\", () => { m.mockImplementationOnce(f); });\n});";
        assert_eq!(queuing_titles(some), Some(Some(HashSet::from(["a", "c"]))));
        let hook = "beforeEach(() => { m.mockResolvedValueOnce(1); });\nit('a', () => {});";
        assert_eq!(
            queuing_titles(hook),
            Some(None),
            "a hook queues for every test"
        );
        let later_hook = "it('a', () => {});\nbeforeEach(() => { m.mockResolvedValueOnce(1); });";
        assert_eq!(queuing_titles(later_hook), Some(None));
        let helper = "function go() { m.mockReturnValueOnce(1); }\nit('a', () => { go(); });";
        assert_eq!(
            queuing_titles(helper),
            Some(None),
            "a helper above the tests may be anyone's"
        );
        let made = "it(`${x} works`, () => { m.mockReturnValueOnce(1); });";
        assert_eq!(
            queuing_titles(made),
            Some(None),
            "a generated title cannot be matched"
        );
        let word = "commit('a'); audit('b');\nit('a', () => { m.mockReturnValueOnce(1); });";
        assert_eq!(queuing_titles(word), Some(Some(HashSet::from(["a"]))));
    }

    #[test]
    fn a_report_names_the_tests_that_queue_values() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("q.spec.ts");
        std::fs::write(
            &spec,
            "it('a', () => { m.mockReturnValueOnce(1); });\nit('b', () => {});\n",
        )
        .unwrap();
        let name = spec.to_string_lossy().replace('\\', "/");
        let tests = r#"[{"ancestorTitles": ["s"], "title": "a", "status": "passed"}, {"ancestorTitles": ["s"], "title": "b", "status": "passed"}]"#;
        let report = dir.path().join("report.json");
        std::fs::write(
            &report,
            format!(r#"{{"testResults": [{{"name": "{name}", "assertionResults": {tests}}}]}}"#),
        )
        .unwrap();
        let read = Report::read(&report, dir.path());
        assert_eq!(
            read.queuing,
            HashSet::from(["s > a".to_string(), "s a".to_string()])
        );
    }

    #[test]
    fn a_mutant_of_a_loading_module_nobody_noticed_runs_with_every_test() {
        let reach = Reach {
            outside: HashSet::from([1]),
            failing: BTreeSet::from(["broken".to_string()]),
            ..Reach::default()
        };
        let report = |test: &str, passed: bool| Report {
            outcome: HashMap::from([(test.to_string(), passed)]),
            ..Report::default()
        };
        let files = ["a.spec.ts".to_string()];
        assert!(needs_every_test(1, &files, &report("t", true), &reach));
        assert!(
            needs_every_test(1, &files, &report("broken", false), &reach),
            "a test that fails anyway noticed nothing"
        );
        assert!(
            !needs_every_test(1, &files, &report("t", false), &reach),
            "killed"
        );
        assert!(
            !needs_every_test(2, &files, &report("t", true), &reach),
            "reached by tests"
        );
        assert!(
            !needs_every_test(1, &[], &report("t", true), &reach),
            "it just ran with every test"
        );
    }

    #[test]
    fn a_pack_failure_whose_mutant_never_took_effect_is_not_its_kill() {
        let outcome = HashMap::from([
            ("a".to_string(), false),
            ("b".to_string(), false),
            ("c".to_string(), true),
        ]);
        let plan = vec![
            ("a".to_string(), 1),
            ("b".to_string(), 2),
            ("c".to_string(), 3),
        ];
        let results = pack_results(outcome, &plan, "a\t1\n");
        assert_eq!(
            results.polluted,
            BTreeSet::from(["b".to_string()]),
            "b failed with its mutant off: what an earlier test left behind"
        );
        assert_eq!(
            results.activated,
            Some(BTreeSet::from([("a".to_string(), 1)]))
        );
    }

    #[test]
    fn a_test_file_that_fails_to_load_is_named_with_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("src").join("case.spec.ts");
        let json = serde_json::json!({ "testResults": [{
            "name": file.to_string_lossy(),
            "assertionResults": [],
            "message": "Cannot find module 'C:/x/src/tests/setup.ts'\n    at resolve",
        }]});
        let path = dir.path().join("report.json");
        std::fs::write(&path, json.to_string()).unwrap();
        let report = Report::read(&path, dir.path());
        assert!(report.outcome.is_empty());
        let failure = report.first_failure.unwrap();
        assert!(
            failure.ends_with("case.spec.ts: Cannot find module 'C:/x/src/tests/setup.ts'"),
            "{failure}"
        );
    }

    #[test]
    fn reports_store_both_test_name_styles() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("src").join("math.spec.ts");
        let json = serde_json::json!({ "testResults": [{
            "name": file.to_string_lossy(),
            "assertionResults": [
                { "ancestorTitles": ["math"], "title": "adds", "status": "passed" },
                { "ancestorTitles": ["math"], "title": "divides", "status": "failed" }
            ]
        }]});
        let path = dir.path().join("report.json");
        std::fs::write(&path, json.to_string()).unwrap();
        let report = Report::read(&path, dir.path());
        assert!(report.outcome["math > adds"]);
        assert!(!report.outcome["math divides"]);
        assert_eq!(
            report.home["math > adds"].replace('\\', "/"),
            "src/math.spec.ts"
        );
    }

    #[test]
    fn finds_the_angular_project_of_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = serde_json::json!({ "projects": {
            "web": { "root": "projects/web", "architect": { "test": { "builder": "@angular/build:unit-test" } } },
            "site": { "root": "projects/site", "architect": { "test": { "builder": "@angular/build:karma" } } }
        }});
        std::fs::write(dir.path().join("angular.json"), workspace.to_string()).unwrap();
        let file = |p: &str| dir.path().join(p);
        let web = angular_project(dir.path(), &file("projects/web/src/a.ts")).unwrap();
        assert_eq!(
            (
                web.name.as_str(),
                web.root.as_str(),
                web.source_root.as_str()
            ),
            ("web", "projects/web", "projects/web/src")
        );
        assert_eq!(
            angular_project(dir.path(), &file("projects/site/src/a.ts")),
            None
        );
    }

    #[test]
    fn a_run_that_cannot_load_its_packages_is_the_machine_not_the_mutant() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(exec::log_name(0)),
            "node:internal/modules/cjs/loader:1386\nError: Cannot find module 'C:\\\\sandbox\\\\web\\\\node_modules\\\\@angular\\\\cli\\\\bin\\\\ng.js'\n",
        )
        .unwrap();
        assert!(packages_missing(dir.path()));
        // A crash of the code under test is a crash.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(
            other.path().join(exec::log_name(0)),
            "TypeError: Cannot read properties of undefined (reading 'send')\n",
        )
        .unwrap();
        assert!(!packages_missing(other.path()));
    }

    #[test]
    fn a_mutant_loaded_under_another_files_test_name_is_reached_outside_tests() {
        let package = Path::new("/sandbox/web");
        let mut reach = Reach::default();
        reach
            .home
            .insert("Auth > fails".into(), "src/auth.spec.ts".into());
        reach
            .home
            .insert("Routes > draws".into(), "src/routes.spec.ts".into());
        // During a test of its own file: that test reaches it.
        add_reach(
            &mut reach,
            "Auth > fails\t1\t/sandbox/web/src/auth.spec.ts",
            package,
        );
        // The routes spec loads the module while the last test name of the
        // auth spec is still set: not that test, the loading of the file.
        add_reach(
            &mut reach,
            "Auth > fails\t2\t/sandbox/web/src/routes.spec.ts",
            package,
        );
        // No test running at all.
        add_reach(&mut reach, "\t3\t/sandbox/web/src/routes.spec.ts", package);
        // A record without a file (an older helper).
        add_reach(&mut reach, "Routes > draws\t4", package);
        assert_eq!(
            reach.by_mutant[&1],
            BTreeSet::from(["Auth > fails".to_string()])
        );
        assert!(!reach.by_mutant.contains_key(&2));
        assert_eq!(reach.outside, HashSet::from([2, 3]));
        assert_eq!(
            reach.outside_files[&2],
            BTreeSet::from(["src/routes.spec.ts".to_string()])
        );
        assert_eq!(
            reach.by_mutant[&4],
            BTreeSet::from(["Routes > draws".to_string()])
        );
        let tests = NodeTests {
            warm: HashMap::new(),
            jobs: 1,
            package: package.to_path_buf(),
            runner: Runner::Vitest,
            script: PathBuf::from("vitest.mjs"),
            angular: Vec::new(),
            sandbox: PathBuf::from("/sandbox"),
            mutated: HashSet::new(),
        };
        // Its solo run needs only the file that loads it.
        assert_eq!(tests.solo_files(2, &reach), ["src/routes.spec.ts"]);
        reach.outside.insert(9);
        assert!(
            tests.solo_files(9, &reach).is_empty(),
            "files not known: every test"
        );
    }

    #[test]
    fn each_angular_project_runs_its_own_test_files() {
        let project = |name: &str, root: &str| AngularProject {
            name: name.into(),
            root: root.into(),
            source_root: format!("{root}/src"),
        };
        let tests = NodeTests {
            warm: HashMap::new(),
            jobs: 1,
            package: PathBuf::from("web"),
            runner: Runner::Angular,
            script: PathBuf::from("ng.js"),
            angular: vec![
                project("core", "projects/core"),
                project("admin", "projects/admin"),
            ],
            sandbox: PathBuf::from("sandbox"),
            mutated: HashSet::new(),
        };
        let names = |runs: Vec<(Option<&AngularProject>, Vec<String>)>| {
            runs.into_iter()
                .map(|(p, files)| (p.map(|p| p.name.clone()).unwrap_or_default(), files))
                .collect::<Vec<_>>()
        };
        // All tests: every project, each with all of its own.
        assert_eq!(
            names(tests.runs(&[])),
            [("core".to_string(), vec![]), ("admin".to_string(), vec![])]
        );
        // Some test files: only the projects that hold them, with their files.
        let files = vec![
            "projects/admin/src/a.spec.ts".to_string(),
            "projects/core/auth/src/b.spec.ts".to_string(),
        ];
        assert_eq!(
            names(tests.runs(&files)),
            [
                ("core".to_string(), vec![files[1].clone()]),
                ("admin".to_string(), vec![files[0].clone()])
            ]
        );
        assert_eq!(worse(TestRun::Failed, TestRun::TimedOut), TestRun::TimedOut);
        assert_eq!(worse(TestRun::Failed, TestRun::Passed), TestRun::Failed);
    }

    #[test]
    fn angular_includes_are_relative_to_the_source_root() {
        let core = AngularProject {
            name: "core".into(),
            root: "projects/core".into(),
            source_root: "projects/core/src".into(),
        };
        assert_eq!(
            core.include("projects/core/src/lib/a.spec.ts"),
            "lib/a.spec.ts"
        );
        // A secondary entry point beside the source root.
        assert_eq!(
            core.include("projects/core/auth/src/lib/b.spec.ts"),
            "../auth/src/lib/b.spec.ts"
        );
    }

    #[test]
    fn jest_patterns_escape_regex_characters() {
        assert_eq!(pattern("src\\a.spec.ts", Runner::Jest), "src/a\\.spec\\.ts");
        assert_eq!(pattern("src\\a.spec.ts", Runner::Vitest), "src/a.spec.ts");
    }

    #[test]
    fn finds_the_runner_from_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"devDependencies":{"vitest":"^2"}}"#,
        )
        .unwrap();
        assert_eq!(runner_of(dir.path()), Some(Runner::Vitest));
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"jest":"29"}}"#,
        )
        .unwrap();
        assert_eq!(runner_of(dir.path()), Some(Runner::Jest));
    }
}
