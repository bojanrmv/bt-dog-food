//! End to end: `dgf mutate --run` on the small xUnit solution in
//! `tests/e2e/dotnet`. Needs git and a .NET 10 SDK; skipped without them, or
//! with DGF_SKIP_E2E=1. The sample's tests leave the boundaries untested, and
//! its Shop.Acceptance.Tests project is the only one that tests the discount
//! boundary, so whether it ran shows in the results.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const SAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/e2e/dotnet");
const EXCLUDE_ACCEPTANCE: &str = "**/*.Acceptance.Tests.csproj";

fn has_tools() -> bool {
    let sdks = Command::new("dotnet").arg("--list-sdks").output();
    let dotnet10 = sdks.is_ok_and(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l.split('.').next().and_then(|m| m.parse::<u32>().ok()) >= Some(10))
    });
    let git = Command::new("git").arg("--version").output().is_ok();
    dotnet10 && git && std::env::var_os("DGF_SKIP_E2E").is_none()
}

/// The sample in a git repository with no history and no cache. It lives in
/// the same folder every time, so its sandbox (and the build output in it)
/// is reused and later test runs build incrementally.
fn repository() -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("dotnet-sample");
    for state in [".git", ".dgf"] {
        if dir.join(state).exists() {
            std::fs::remove_dir_all(dir.join(state)).unwrap();
        }
    }
    std::fs::create_dir_all(&dir).unwrap();
    copy(Path::new(SAMPLE), &dir);
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args([
                "-c",
                "user.email=e2e@dgf",
                "-c",
                "user.name=e2e",
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(status.status.success(), "{status:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "sample"]);
    dir
}

/// Copies the files whose content differs, so the others keep their time.
fn copy(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy(&entry.path(), &target);
        } else if std::fs::read(&target).ok() != std::fs::read(entry.path()).ok() {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn dgf(repo: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dgf"));
    command
        .args(["mutate", "src", "--run", "--format", "json"])
        .args(args)
        .current_dir(repo);
    command
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "{err}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// `dgf mutate --format json` without --run: the chosen functions.
fn listing(repo: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_dgf"))
        .args(["mutate", "--format", "json"])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    report(&output)
}

fn run(repo: &Path, args: &[&str]) -> (i32, Value) {
    let output = dgf(repo, args).output().unwrap();
    (output.status.code().unwrap_or(-1), report(&output))
}

/// The outcome of the mutant of `function` at `line` written `change`: the
/// original (`>`), or the original and its replacement (`> -> >=`).
fn outcome<'a>(report: &'a Value, function: &str, line: u64, change: &str) -> &'a str {
    mutant(report, function, line, change)["outcome"]
        .as_str()
        .unwrap_or("none")
}

fn mutant<'a>(report: &'a Value, function: &str, line: u64, change: &str) -> &'a Value {
    let (original, replacement) = change.split_once(" -> ").unwrap_or((change, ""));
    let functions = report["functions"].as_array().unwrap();
    let found = functions
        .iter()
        .filter(|f| f["name"] == function)
        .flat_map(|f| f["mutants"].as_array().unwrap())
        .find(|m| {
            m["mutant"]["line"] == line
                && m["mutant"]["original"] == original
                && (replacement.is_empty() || m["mutant"]["replacement"] == replacement)
        });
    found.unwrap_or_else(|| panic!("no mutant {change} at {function}:{line} in {report}"))
}

fn sandbox(report: &Value) -> PathBuf {
    PathBuf::from(report["sandboxes"][0].as_str().unwrap())
}

/// Every mutant of the C# mutator fixtures, switched in at once, compiles:
/// a switch in a wrong place would otherwise cost a rebuild per run.
/// `Modern.cs` needs C# 15, so a .NET 11 SDK.
#[test]
fn every_csharp_fixture_mutant_compiles() {
    if !has_tools() {
        eprintln!("skipped: needs git and a .NET 10 SDK (or DGF_SKIP_E2E is set)");
        return;
    }
    compile_switched("Mutators.cs", "net10.0");
    if has_sdk(11) {
        compile_switched("Modern.cs", "net11.0");
    } else {
        eprintln!("Modern.cs skipped: needs a .NET 11 SDK");
    }
}

fn has_sdk(major: u32) -> bool {
    Command::new("dotnet")
        .arg("--list-sdks")
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split('.').next().and_then(|m| m.parse::<u32>().ok()) >= Some(major))
        })
}

fn compile_switched(fixture: &str, framework: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dog-food-core/tests/mutants/csharp")
        .join(fixture);
    let source = std::fs::read_to_string(&path).unwrap();
    let settings = dog_food_core::mutate::Settings {
        level: dog_food_core::mutate::Level::Advanced,
        ..Default::default()
    };
    let found =
        dog_food_core::mutate::mutate(&source, dog_food_core::Language::CSharp, &settings).unwrap();
    let active: Vec<&dog_food_core::Mutant> = found
        .mutants
        .iter()
        .filter(|m| m.ignored.is_none())
        .collect();
    assert!(active.len() > 40, "{fixture}: {} mutants", active.len());
    let switched =
        dog_food_core::mutate::schemata(&source, dog_food_core::Language::CSharp, &active, &|id| {
            format!("Dgf.Is({id})")
        });
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("fixture-compile")
        .join(framework);
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, text: &str| {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            std::fs::write(path, text).unwrap();
        }
    };
    write(
        "Fixture.csproj",
        &format!("<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>{framework}</TargetFramework>\n    <Nullable>enable</Nullable>\n    <TreatWarningsAsErrors>false</TreatWarningsAsErrors>\n    <NoWarn>$(NoWarn);CS0162;CS0219;CS1998;CS8600;CS8602;CS8603;CS8604;CS8618;CS8625</NoWarn>\n  </PropertyGroup>\n</Project>\n"),
    );
    write(
        "Dgf.cs",
        "public static class Dgf { public static bool Is(int id) => System.Environment.GetEnvironmentVariable(\"DGF\") == id.ToString(); }\n",
    );
    write(fixture, &switched);
    let output = Command::new("dotnet")
        .args(["build", "-nologo", "-v", "q", "-p:RunAnalyzers=false"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{fixture} with every mutant switched in does not compile:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn mutation_runs_on_a_dotnet_solution() {
    if !has_tools() {
        eprintln!("skipped: needs git and a .NET 10 SDK (or DGF_SKIP_E2E is set)");
        return;
    }
    let root = &repository();

    // Leaving the acceptance tests out: the discount boundary survives.
    let (code, first) = run(root, &["--exclude-test-project", EXCLUDE_ACCEPTANCE]);
    assert_eq!(code, 0, "{first}");
    let projects = first["testProjects"].to_string();
    assert!(
        projects.contains("Shop.Tests") && !projects.contains("Acceptance"),
        "{projects}"
    );
    assert_eq!(
        outcome(&first, "Pricing.Discount", 10, "> -> >="),
        "survived"
    );
    assert_eq!(
        outcome(&first, "Pricing.IsActive", 35, "<= -> <"),
        "survived"
    );
    assert_eq!(
        outcome(&first, "Pricing.Shipping", 21, "weight <= 0 -> false"),
        "killed"
    );
    assert_eq!(outcome(&first, "Unused.Twice", 5, "*"), "no-coverage");
    // Built in a static field initializer, inside whichever test touches the
    // class first: every test runs with it, so the one that checks the values kills it.
    assert_eq!(outcome(&first, "Codes.Build", 17, "* -> /"), "killed");
    // Filled by a lazy cache on first use: the same, through GetOrAdd.
    assert_eq!(
        outcome(&first, "Codes.Square/lambda@23:60", 23, "* -> /"),
        "killed"
    );
    // The source generator behind [GeneratedRegex] still ran: nothing failed to build.
    assert_eq!(first["tally"]["compileError"], 0, "{first}");
    // The guard clause's comment turns its statement mutant off, with its reason.
    let guard = mutant(
        &first,
        "Pricing.Shipping",
        23,
        "throw new ArgumentOutOfRangeException(nameof(weight)); -> ;",
    );
    assert_eq!(guard["outcome"], "ignored", "{guard}");
    assert!(
        guard["mutant"]["ignored"]
            .as_str()
            .is_some_and(|why| why.starts_with("a guard clause")),
        "{guard}"
    );
    assert_eq!(first["tally"]["ignored"], 1, "{first}");
    let shipping_function = first["functions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "Pricing.Shipping")
        .unwrap();
    assert_eq!(shipping_function["suppressions"][0]["line"], 23);
    assert_eq!(shipping_function["suppressions"][0]["mutants"], 1);

    // The sandbox holds the tested projects and the build files they need, nothing else.
    let copy = sandbox(&first);
    for wanted in [
        "src/Shop/Pricing.cs",
        "tests/Shop.Tests/PricingTests.cs",
        "build/Shared.props",
        "Directory.Build.props",
        "global.json",
    ] {
        assert!(
            copy.join(wanted).exists(),
            "{wanted} missing from {}",
            copy.display()
        );
    }
    for unwanted in ["docs", "tests/Shop.Acceptance.Tests", ".git"] {
        assert!(
            !copy.join(unwanted).exists(),
            "{unwanted} copied to {}",
            copy.display()
        );
    }

    // Nothing changed: everything comes from the cache.
    let (_, again) = run(root, &["--exclude-test-project", EXCLUDE_ACCEPTANCE]);
    // Ignored mutants are never tested, so never cached.
    let total = again["functions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["mutants"].as_array().unwrap())
        .filter(|m| m["outcome"] != "ignored")
        .count();
    assert_eq!(again["cached"], total, "{again}");

    // Below the break threshold the run fails, and says so.
    let (code, _) = run(
        root,
        &[
            "--exclude-test-project",
            EXCLUDE_ACCEPTANCE,
            "--fail-under",
            "99",
        ],
    );
    assert_eq!(code, 1);

    // Survivors are tested again on request; kills stay cached.
    let (_, rerun) = run(
        root,
        &[
            "--exclude-test-project",
            EXCLUDE_ACCEPTANCE,
            "--rerun-survivors",
        ],
    );
    let undetected = again["functions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["mutants"].as_array().unwrap())
        .filter(|m| m["outcome"] == "survived" || m["outcome"] == "no-coverage")
        .count();
    assert!(undetected > 0);
    assert_eq!(rerun["cached"], total - undetected, "{rerun}");
    assert_eq!(
        outcome(&rerun, "Pricing.Discount", 10, "> -> >="),
        "survived"
    );

    // A shipping test changed: kills by pricing tests still hold, survivors are tested again.
    let shipping = root.join("tests/Shop.Tests/ShippingTests.cs");
    let text = std::fs::read_to_string(&shipping).unwrap();
    std::fs::write(&shipping, text + "\n// changed\n").unwrap();
    let (_, changed) = run(root, &["--exclude-test-project", EXCLUDE_ACCEPTANCE]);
    assert_eq!(
        mutant(
            &changed,
            "Pricing.Discount",
            10,
            "member && total > 100 -> false"
        )["cached"],
        true,
        "{changed}"
    );
    assert_eq!(
        mutant(&changed, "Pricing.IsActive", 35, "<= -> <")["cached"],
        false,
        "{changed}"
    );
    assert_eq!(
        outcome(&changed, "Pricing.IsActive", 35, "<= -> <"),
        "survived"
    );

    // A branch that only changes a test brings back the functions it reaches.
    let listed = listing(root, &["--changes", "--base", "HEAD"]);
    let reasons: Vec<(String, String)> = listed["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_string(),
                t["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        reasons.contains(&("Pricing.Shipping".into(), "tests".into())),
        "{reasons:?}"
    );

    // --scope lines keeps only the mutants on changed lines.
    let pricing = root.join("src/Shop/Pricing.cs");
    let text = std::fs::read_to_string(&pricing).unwrap();
    let edited = text.replacen(
        "return discount > 12 ? 12 : discount;",
        "return discount > 12 ? 12 : discount; // capped",
        1,
    );
    assert_ne!(edited, text);
    std::fs::write(&pricing, edited).unwrap();
    let lines = listing(root, &["--changes", "--base", "HEAD", "--scope", "lines"]);
    let discount = lines["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Pricing.Discount")
        .unwrap();
    let mutant_lines: Vec<u64> = discount["mutants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["line"].as_u64().unwrap())
        .collect();
    assert!(
        !mutant_lines.is_empty() && mutant_lines.iter().all(|l| *l == 13),
        "{mutant_lines:?}"
    );

    // One mutant per test run, at low priority, with fewer build nodes: the
    // same verdicts as packed runs.
    let (_, alone) = run(
        root,
        &[
            "--no-cache",
            "--exclude-test-project",
            EXCLUDE_ACCEPTANCE,
            "--no-mix",
            "--low-priority",
            "--build-jobs",
            "2",
        ],
    );
    assert_eq!(
        outcome(&alone, "Pricing.Discount", 10, "> -> >="),
        "survived"
    );
    assert_eq!(
        outcome(&alone, "Pricing.Shipping", 21, "weight <= 0 -> false"),
        "killed"
    );
    // One run per mutant, a crashed one included, plus a second, longer run
    // for each that timed out (a busy machine or an endless loop).
    let tally = |name: &str| alone["tally"][name].as_u64().unwrap();
    let tested = tally("killed") + tally("survived") + tally("timeout") + tally("runtimeError");
    let retries = alone["timings"]["retries"].as_u64().unwrap();
    assert_eq!(
        alone["timings"]["testRuns"].as_u64().unwrap(),
        tested + retries,
        "{alone}"
    );

    // Two runs at once: a test filter, and the acceptance tests included.
    let filtered = dgf(
        root,
        &[
            "--no-cache",
            "--exclude-test-project",
            EXCLUDE_ACCEPTANCE,
            "--test-filter",
            "FullyQualifiedName~ShippingTests",
        ],
    )
    .spawn_output();
    let everything = dgf(root, &["--no-cache"]).spawn_output();
    let (filtered, everything) = (report(&filtered.wait()), report(&everything.wait()));
    assert_eq!(
        outcome(&filtered, "Pricing.Discount", 10, "> -> >="),
        "no-coverage"
    );
    assert_eq!(
        outcome(&filtered, "Pricing.Shipping", 21, "weight <= 0 -> false"),
        "killed"
    );
    assert_eq!(
        outcome(&everything, "Pricing.Discount", 10, "> -> >="),
        "killed"
    );
    assert_ne!(sandbox(&filtered), sandbox(&everything));

    // No time left: exit code 4, nothing tested, nothing lost.
    let (code, late) = run(root, &["--no-cache", "--max-minutes", "0"]);
    assert_eq!(code, 4, "{late}");
    assert_eq!(late["budgetHit"], true);
    assert_eq!(late["tally"]["untested"], total, "{late}");
}

/// A command started now, waited for later.
trait SpawnOutput {
    fn spawn_output(&mut self) -> Running;
}

struct Running(std::process::Child);

impl Running {
    fn wait(self) -> Output {
        self.0.wait_with_output().unwrap()
    }
}

impl SpawnOutput for Command {
    fn spawn_output(&mut self) -> Running {
        Running(
            self.stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        )
    }
}
