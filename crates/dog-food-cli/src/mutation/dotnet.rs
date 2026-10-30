//! The C# engine: `dotnet build`, and `dotnet test` with dog-food's
//! in-process collector, so every test reports its name to the mutant
//! switches and writes its own result.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use super::exec::{self, fresh_folder, lines_in, write_plan, Located};
use super::projects::TestProject;
use super::sandbox::FileMutants;
use super::{
    check_baseline, failing_tests, Budget, BuildError, Engine, Pack, PackRun, Reach, Round,
    SoloRun, TestRun,
};
use crate::discover::absolute;

const COLLECTOR_CS: &str = include_str!("../../assets/collector/DgfCollector.cs");
const COLLECTOR_PROJECT: &str = include_str!("../../assets/collector/DgfCollector.csproj");
/// Longest a build or the baseline test run may take.
const HOUR: Duration = Duration::from_secs(3600);
/// The subfolder of a run's folder where the collector writes test results.
const RESULTS: &str = "results";

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
}

/// Where a sandbox records the build settings of its last successful build.
pub const RESTORED: &str = ".dgf-restored";

impl Dotnet {
    /// Uses the test projects `chosen` (relative to the repository, which the
    /// sandbox mirrors).
    pub fn prepare(
        sandbox: &Path,
        files: &[FileMutants],
        chosen: &[TestProject],
        settings: u64,
        build_args: Vec<String>,
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
        })
    }

    /// Runs the given projects (with a test filter each) and collects every
    /// test's result from the collector; side by side when `parallel`.
    fn run(
        &self,
        runs: &[(&Project, Option<String>)],
        env: &[(&str, String)],
        limit: Duration,
        folder: &Path,
        parallel: bool,
    ) -> Result<(TestRun, Results), String> {
        let one = |(index, (project, filter)): (usize, &(&Project, Option<String>))| {
            let mut command = self.test_command(project, filter.as_deref(), folder, index)?;
            command.envs(env.iter().map(|(k, v)| (*k, v.as_str())));
            let started = Instant::now();
            let status = exec::run(command, limit, &folder.join(exec::log_name(index)))?;
            Ok::<_, String>((status, project.assembly.clone(), started.elapsed()))
        };
        let statuses: Vec<Result<(TestRun, String, Duration), String>> = if parallel {
            runs.par_iter().enumerate().map(one).collect()
        } else {
            runs.iter().enumerate().map(one).collect()
        };
        let mut status = TestRun::Passed;
        let mut walls = HashMap::new();
        for run in statuses {
            let (one_status, assembly, wall) = run?;
            status = worst(status, one_status);
            walls.insert(assembly, wall);
        }
        if status == TestRun::TimedOut {
            return Ok((status, Results::default()));
        }
        let mut results = Results::read(&folder.join(RESULTS), &self.sandbox);
        results.walls = walls;
        Ok((status, results))
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
        let results = folder.join(RESULTS);
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
        let folder = fresh_folder(&self.sandbox, "baseline")?;
        let reached = folder.join("reach");
        std::fs::create_dir_all(&reached).map_err(|err| err.to_string())?;
        let started = Instant::now();
        let runs = self.runs_for(None, &Reach::default());
        let env = [("DGF_REACH", reached.to_string_lossy().into_owned())];
        let (status, results) = self.run(&runs, &env, budget.limit(HOUR), &folder, true)?;
        if status == TestRun::TimedOut && budget.spent() {
            return Ok(None);
        }
        check_baseline(status, &results.outcome, Some(&folder))?;
        let mut reach = Reach {
            failing: failing_tests(&results.outcome),
            files: self.test_files(&results),
            startup: startup(&results.walls, &results.home, &results.times),
            home: results.home,
            times: results.times,
            duration: started.elapsed(),
            ..Reach::default()
        };
        record_reach(&mut reach, &lines_in(&reached));
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
        let env = [("DGF_PLAN", plan.to_string_lossy().into_owned())];
        let (status, results) = self.run(&runs, &env, limit, &folder, false)?;
        Ok((status != TestRun::TimedOut).then_some(results.outcome))
    }

    fn solo(
        &self,
        mutant: usize,
        reach: &Reach,
        limit: Duration,
        tag: &str,
    ) -> Result<SoloRun, String> {
        let folder = fresh_folder(&self.sandbox, tag)?;
        let tests = (!reach.outside.contains(&mutant))
            .then(|| reach.by_mutant.get(&mutant))
            .flatten();
        let runs = self.runs_for(tests, reach);
        let (status, results) = self.run(
            &runs,
            &[("DGF_MUTANT", mutant.to_string())],
            limit,
            &folder,
            false,
        )?;
        Ok((status, results.outcome))
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
}

impl Results {
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

/// Reach lines "test<TAB>mutant"; an empty test means outside any test.
fn record_reach(reach: &mut Reach, lines: &[String]) {
    for line in lines {
        let Some((test, id)) = line.rsplit_once('\t') else {
            continue;
        };
        let Ok(id) = id.trim().parse::<usize>() else {
            continue;
        };
        if test.is_empty() {
            reach.outside.insert(id);
        } else {
            reach
                .by_mutant
                .entry(id)
                .or_default()
                .insert(test.to_string());
        }
    }
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

/// Builds dog-food's collector once per version of its source; other dgf
/// processes wait while one builds it.
fn collector() -> Result<PathBuf, String> {
    let version = super::fnv(&[COLLECTOR_CS.as_bytes(), COLLECTOR_PROJECT.as_bytes()]);
    let dir = super::sandbox::temp_root()
        .join("dgf-mutate")
        .join(format!("collector-{version:016x}"));
    let dll = dir.join("out").join("DgfCollector.dll");
    std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    let lock = std::fs::File::create(dir.with_extension("lock")).map_err(|err| err.to_string())?;
    lock.lock().map_err(|err| err.to_string())?;
    if dll.exists() {
        return Ok(dll);
    }
    std::fs::write(dir.join("DgfCollector.cs"), COLLECTOR_CS).map_err(|err| err.to_string())?;
    std::fs::write(dir.join("DgfCollector.csproj"), COLLECTOR_PROJECT)
        .map_err(|err| err.to_string())?;
    let out = dir.join("out");
    let build = || {
        dotnet(&[
            "build",
            &dir.join("DgfCollector.csproj").to_string_lossy(),
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
            "could not build dog-food's test collector:\n{}{}",
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
            &[
                "A.one\t3".into(),
                "A.two\t3".into(),
                "\t5".into(),
                "junk".into(),
            ],
        );
        assert_eq!(reach.by_mutant[&3].len(), 2);
        assert!(reach.outside.contains(&5));
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
