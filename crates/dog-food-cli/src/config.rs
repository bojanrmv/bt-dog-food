//! `dgf.toml`: project settings, found in the current folder or a parent.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::changes::{FailOn, Scope};

pub const FILE_NAME: &str = "dgf.toml";

/// Written by `dgf init`.
pub const TEMPLATE: &str = r#"# dog-food settings. `dgf` looks for this file in the current folder and its parents.

[analyze]
# Files and folders to skip, in .gitignore syntax, relative to this file.
# .gitignore is always respected, and *.d.ts, *.g.cs and *.Designer.cs are always skipped.
exclude = [
  # "tests/fixtures/",
  # "**/generated/",
]

# Rows to show in the table (0 = all).
# top = 20

[limits]
# Functions scoring above `warn` are flagged. Functions above `fail` make dgf
# exit with code 1, which fails a CI build. Without `fail`, dgf never fails.
warn = 10
# fail = 20

# Different limits for some paths (.gitignore syntax, relative to this file).
# Unset values fall back to the ones above; if several overrides match a file,
# the last one wins.
# [[limits.overrides]]
# paths = ["legacy/", "**/tests/"]
# warn = 20
# fail = 40

[changes]
# `dgf changes` compares your branch and uncommitted edits with this branch.
# base = "main"
# "lines": functions containing a changed line; "files": every function in a changed file.
# scope = "lines"
# "new-or-worse": only new functions and higher scores can fail; "touched": any changed function.
# fail_on = "new-or-worse"
# Limits for `dgf changes` only (the hook and PR gates), over [limits]: new code
# can be held to more than old code. `fail = 10` fails a new function above 10.
# fail = 10

[history]
# Where `dgf analyze --save-history` keeps snapshots, relative to this file.
# dir = ".dgf/history"

[coverage]
# Coverage reports (Cobertura XML or LCOV) or folders to search for them, relative
# to this file. With coverage, every function also gets a CRAP score.
# paths = ["TestResults", "coverage/lcov.info"]

[mutate]
# Files `dgf mutate` never mutates (.gitignore syntax, relative to this file).
# exclude = ["**/Migrations/", "**/Program.cs"]
# `dotnet test --filter` for the mutation runs: "filter" for every test project,
# or "Project.Tests:filter" for one. With per-project filters, only those projects run.
# test_filter = ["Shop.Tests:FullyQualifiedName~PricingTests"]
# Test projects to use or leave out (glob patterns on the .csproj path).
# test_projects = ["**/*.Tests.csproj"]
# exclude_test_projects = ["**/*.Acceptance.Tests.csproj", "**/*.IntegrationTests.csproj"]
# Where results are kept; an absolute path shares them between clones and worktrees.
# cache_dir = ".dgf/mutation-cache"
# Stop after this many minutes, keep what was tested, and exit with code 4.
# max_minutes = 45
# "closure": copy only the projects the tests need (default); "full": the whole repository.
# sandbox = "closure"
# Which mutators run: basic, standard (default), advanced.
# level = "standard"
# Mutators never made, by name.
# ignore_mutators = ["string"]
# Calls left alone with everything inside them; `*` stays within one name part.
# ignore_methods = ["*Log*", "ToString", "ConfigureAwait", "Dispose"]
# Leave out constructors that only store their arguments (dependency injection).
# skip_plain_constructors = true
# A test run hangs after (startup + its tests' time without mutants) x factor + ms.
# timeout_factor = 1.5
# timeout_ms = 5000
# Test runs at the same time (default: CPU cores - 1), `dotnet build -m:N`, and a
# lower priority so other work on the machine stays responsive.
# jobs = 3
# build_jobs = 2
# low_priority = true
# Score bands: high and low color the report; below break, dgf exits with code 1.
# thresholds = { high = 80, low = 60, break = 50 }
"#;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub analyze: AnalyzeConfig,
    pub limits: LimitsConfig,
    pub changes: ChangesConfig,
    pub history: HistoryConfig,
    pub coverage: CoverageConfig,
    pub mutate: MutateConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MutateConfig {
    /// Files never mutated, .gitignore syntax, relative to dgf.toml.
    pub exclude: Vec<String>,
    /// `dotnet test` filters: "filter" or "Project:filter".
    #[serde(deserialize_with = "one_or_many")]
    pub test_filter: Vec<String>,
    /// Glob patterns of test projects to use; all when empty.
    pub test_projects: Vec<String>,
    /// Glob patterns of test projects to leave out.
    pub exclude_test_projects: Vec<String>,
    /// Where results are cached, relative to dgf.toml.
    pub cache_dir: Option<String>,
    pub max_minutes: Option<f64>,
    pub sandbox: Option<SandboxMode>,
    /// `basic`, `standard` (default) or `advanced`.
    pub level: Option<String>,
    /// Mutators never made, by name.
    pub ignore_mutators: Vec<String>,
    /// Calls left alone with everything inside them.
    pub ignore_methods: Vec<String>,
    /// Constructors that only store their arguments in fields are not
    /// mutated: a removed assignment fails every test that uses the object.
    pub skip_plain_constructors: bool,
    /// A test run hangs after (startup + its tests' time) × `timeout_factor`
    /// + `timeout_ms`; defaults 1.5 and 5000.
    pub timeout_ms: Option<u64>,
    pub timeout_factor: Option<f64>,
    pub thresholds: Thresholds,
    /// Test runs at the same time (default: cores - 1).
    pub jobs: Option<usize>,
    /// `dotnet build -m:N`.
    pub build_jobs: Option<usize>,
    /// Run at below-normal priority.
    pub low_priority: bool,
}

/// Mutation score bands: `high` and `low` color
/// the report; a score below `break` fails the run.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    pub high: f64,
    pub low: f64,
    #[serde(rename = "break")]
    pub fail: Option<f64>,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            high: 80.0,
            low: 60.0,
            fail: None,
        }
    }
}

/// How much of the repository the mutation sandbox copies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// The projects the chosen tests build, and the files those projects use.
    #[default]
    Closure,
    /// The whole repository.
    Full,
}

/// A string or a list of strings.
fn one_or_many<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(one) => vec![one],
        OneOrMany::Many(many) => many,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CoverageConfig {
    /// Coverage reports or folders to search, relative to dgf.toml.
    pub paths: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChangesConfig {
    pub base: Option<String>,
    pub scope: Option<Scope>,
    pub fail_on: Option<FailOn>,
    /// Limits for `dgf changes` only, over [limits]: a gate on new code can
    /// be stricter than what `dgf analyze` asks of old code.
    pub warn: Option<u32>,
    pub fail: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    pub dir: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnalyzeConfig {
    pub exclude: Vec<String>,
    pub top: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    pub warn: Option<u32>,
    pub fail: Option<u32>,
    /// CRAP score limits, applied when coverage is loaded (default: warn above 30).
    pub crap_warn: Option<f64>,
    pub crap_fail: Option<f64>,
    pub overrides: Vec<LimitOverride>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitOverride {
    pub paths: Vec<String>,
    pub warn: Option<u32>,
    pub fail: Option<u32>,
}

/// The nearest `dgf.toml` in `start` or one of its parents.
pub fn find(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join(FILE_NAME))
        .find(|path| path.is_file())
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    parse(&text).map_err(|err| format!("invalid {}: {err}", path.display()))
}

pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults() {
        let config = parse(TEMPLATE).unwrap();
        assert!(config.analyze.exclude.is_empty());
        assert_eq!(config.analyze.top, None);
        assert_eq!(config.limits.warn, Some(10));
        assert_eq!(config.limits.fail, None);
        assert!(config.limits.overrides.is_empty());
        assert_eq!(config.changes.base, None);
        assert_eq!(config.history.dir, None);
    }

    #[test]
    fn reads_changes_and_history_settings() {
        let config = parse(
            "[changes]
base = \"develop\"
scope = \"files\"
fail_on = \"touched\"
             [history]
dir = \"reports/history\"",
        )
        .unwrap();
        assert_eq!(config.changes.base.as_deref(), Some("develop"));
        assert_eq!(config.changes.scope, Some(Scope::Files));
        assert_eq!(config.changes.fail_on, Some(FailOn::Touched));
        assert_eq!(config.history.dir.as_deref(), Some("reports/history"));
    }

    #[test]
    fn reads_mutate_settings() {
        let config = parse(
            "[mutate]\ntest_filter = \"FullyQualifiedName~Pricing\"\n\
             exclude_test_projects = [\"**/*.Acceptance.Tests.csproj\"]\n\
             max_minutes = 45\nsandbox = \"full\"\nlevel = \"basic\"\n\
             ignore_methods = [\"*Log*\"]\nignore_mutators = [\"string\"]",
        )
        .unwrap();
        assert_eq!(config.mutate.level.as_deref(), Some("basic"));
        let banded =
            parse("[mutate]\nthresholds = { high = 90, low = 70, break = 50 }\ntimeout_ms = 3000")
                .unwrap();
        assert_eq!(banded.mutate.thresholds.fail, Some(50.0));
        assert_eq!(banded.mutate.thresholds.high, 90.0);
        assert_eq!(banded.mutate.timeout_ms, Some(3000));
        assert_eq!(parse("").unwrap().mutate.thresholds.low, 60.0);
        assert_eq!(config.mutate.ignore_methods, ["*Log*"]);
        assert_eq!(config.mutate.ignore_mutators, ["string"]);
        assert_eq!(config.mutate.test_filter, ["FullyQualifiedName~Pricing"]);
        assert_eq!(config.mutate.exclude_test_projects.len(), 1);
        assert_eq!(config.mutate.max_minutes, Some(45.0));
        assert_eq!(config.mutate.sandbox, Some(SandboxMode::Full));
        let many = parse("[mutate]\ntest_filter = [\"A:x\", \"B:y\"]").unwrap();
        assert_eq!(many.mutate.test_filter, ["A:x", "B:y"]);
    }

    #[test]
    fn empty_file_is_valid() {
        assert!(parse("").unwrap().analyze.exclude.is_empty());
    }

    #[test]
    fn reads_all_settings() {
        let config = parse(
            "[analyze]\nexclude = [\"a/\", \"**/b/\"]\ntop = 5\n\
             [limits]\nwarn = 8\nfail = 12\n\
             [[limits.overrides]]\npaths = [\"legacy/\"]\nfail = 30",
        )
        .unwrap();
        assert_eq!(config.analyze.exclude, ["a/", "**/b/"]);
        assert_eq!(config.analyze.top, Some(5));
        assert_eq!(config.limits.warn, Some(8));
        assert_eq!(config.limits.fail, Some(12));
        assert_eq!(config.limits.overrides[0].paths, ["legacy/"]);
        assert_eq!(config.limits.overrides[0].warn, None);
        assert_eq!(config.limits.overrides[0].fail, Some(30));
    }

    #[test]
    fn override_needs_paths() {
        let err = parse("[[limits.overrides]]\nfail = 30").unwrap_err();
        assert!(err.to_string().contains("paths"), "{err}");
    }

    #[test]
    fn unknown_settings_are_rejected() {
        let err = parse("[analyze]\nexcludes = []").unwrap_err();
        assert!(err.to_string().contains("excludes"), "{err}");
    }

    #[test]
    fn find_walks_up_to_parent_folders() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(find(&nested), None);

        std::fs::write(root.path().join(FILE_NAME), "").unwrap();
        assert_eq!(find(&nested), Some(root.path().join(FILE_NAME)));
    }
}
