//! The TypeScript/JavaScript engine: Vitest, Jest, or Angular's unit-test
//! builder (`ng test`, Vitest inside), run through Node with their JSON
//! reporters. There is no build step; the test runners compile TypeScript as
//! they load it. The mutant switches find the running test
//! through the test framework's `expect` state.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::exec::{self, error_location, fresh_folder, lines_in, write_plan, Located};
use super::sandbox::FileMutants;
use super::{
    check_baseline, failing_tests, Budget, BuildError, Engine, Pack, PackRun, Reach, Round,
    SoloRun, TestRun,
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
    /// Angular only: the project.
    angular: Option<AngularProject>,
    sandbox: PathBuf,
    /// The mutated files: compiler errors elsewhere are not the mutants' doing.
    mutated: HashSet<PathBuf>,
}

impl NodeTests {
    /// Uses the nearest package.json above the mutated files that depends on
    /// Vitest or Jest.
    pub fn prepare(sandbox: &Path, files: &[FileMutants]) -> Result<NodeTests, String> {
        let first = files.first().ok_or("no TypeScript or JavaScript mutants")?;
        let start = sandbox.join(&first.relative);
        let (package, runner) = start
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(sandbox))
            .find_map(|dir| runner_of(dir).map(|r| (dir.to_path_buf(), r)))
            .ok_or("no package.json with vitest or jest above the mutated files")?;
        let angular = angular_project(&package, &start);
        let runner = if angular.is_some() {
            Runner::Angular
        } else {
            runner
        };
        let script = script_for(&package, runner)
            .ok_or("the test runner is not installed (run npm install in the repository)")?;
        Ok(NodeTests {
            package,
            runner,
            script,
            angular,
            sandbox: sandbox.to_path_buf(),
            mutated: files
                .iter()
                .map(|f| absolute(&sandbox.join(&f.relative)))
                .collect(),
        })
    }

    fn command(&self, report: &Path, files: &[String]) -> Command {
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
                let project = self.angular.as_ref().expect("an Angular project");
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

    /// Runs the given test files (all when empty) and reads every test's result.
    fn run(
        &self,
        files: &[String],
        env: &[(&str, String)],
        limit: Duration,
        folder: &Path,
    ) -> Result<(TestRun, Report), String> {
        let report = folder.join("report.json");
        let mut command = self.command(&report, files);
        command.envs(env.iter().map(|(k, v)| (*k, v.as_str())));
        let status = exec::run(command, limit, &folder.join(exec::log_name(0)))?;
        let parsed = Report::read(&report, &self.package);
        Ok((status, parsed))
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

impl Engine for NodeTests {
    /// Vitest and Jest strip types without checking them, but Angular's
    /// builder type-checks, and a mutant can break narrowing (`typeof a ===
    /// "number" && ...` turned into `||`): tsc finds those first.
    fn build(&self, _: Round, budget: Budget) -> Result<(), BuildError> {
        let Some(project) = &self.angular else {
            return Ok(());
        };
        let config = self.package.join(&project.root).join("tsconfig.spec.json");
        let tsc = script_at(&self.package, "node_modules/typescript/bin/tsc");
        let (true, Some(tsc)) = (config.exists(), tsc) else {
            return Ok(());
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
        let (status, report) = self.run(&[], &env, budget.limit(HOUR), &folder)?;
        if status == TestRun::TimedOut && budget.spent() {
            return Ok(None);
        }
        if report.outcome.is_empty() {
            if let Some(failure) = &report.first_failure {
                return Err(format!(
                    "no test ran without mutants; the first test file that failed: {failure} (all results: {})",
                    folder.join("report.json").display()
                ));
            }
        }
        check_baseline(status, &report.outcome)?;
        let wall = started.elapsed();
        // Each name is stored twice (Vitest's and Jest's spelling).
        let tests: Duration = report.times.values().sum::<Duration>() / 2;
        let startup = report
            .home
            .values()
            .map(|file| (file.clone(), wall.saturating_sub(tests)))
            .collect();
        let package = self
            .package
            .strip_prefix(&self.sandbox)
            .unwrap_or(Path::new(""));
        let mut reach = Reach {
            failing: failing_tests(&report.outcome),
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
        for line in lines_in(&reached) {
            add_reach(&mut reach, &line);
        }
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
        let files = self.files_for(&tests, reach);
        let (status, report) = self.run(
            &files,
            &[("DGF_PLAN", plan.to_string_lossy().into_owned())],
            limit,
            &folder,
        )?;
        Ok((status != TestRun::TimedOut).then_some(report.outcome))
    }

    fn solo(
        &self,
        mutant: usize,
        reach: &Reach,
        limit: Duration,
        tag: &str,
    ) -> Result<SoloRun, String> {
        let files = match reach.by_mutant.get(&mutant) {
            Some(tests) if !reach.outside.contains(&mutant) => self.files_for(tests, reach),
            _ => Vec::new(),
        };
        let folder = fresh_folder(&self.sandbox, tag)?;
        let (status, report) = self.run(
            &files,
            &[("DGF_MUTANT", mutant.to_string())],
            limit,
            &folder,
        )?;
        Ok((status, report.outcome))
    }

    fn test_projects(&self) -> Vec<String> {
        let runner = match (self.runner, &self.angular) {
            (Runner::Angular, Some(project)) => format!("ng test {}", project.name),
            (Runner::Jest, _) => "jest".into(),
            _ => "vitest".into(),
        };
        vec![format!("{} ({runner})", self.package.display())]
    }

    fn crash_reason(&self, tag: &str) -> Option<String> {
        exec::crash_reason(&exec::runs_folder(&self.sandbox).join(tag))
    }
}

fn add_reach(reach: &mut Reach, line: &str) {
    let Some((test, id)) = line.rsplit_once('\t') else {
        return;
    };
    let Ok(id) = id.trim().parse::<usize>() else {
        return;
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
            for test in file.tests {
                report.add(&test, &home);
            }
        }
        report
    }

    fn add(&mut self, test: &JsonTest, home: &str) {
        let mut parts = test.ancestors.clone();
        parts.push(test.title.clone());
        let passed = test.status != "failed";
        let time = Duration::from_secs_f64(test.duration.unwrap_or(0.0).max(0.0) / 1000.0);
        for name in [parts.join(" > "), parts.join(" ")] {
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
