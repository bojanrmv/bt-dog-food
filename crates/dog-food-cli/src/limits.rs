//! Acceptable complexity: a `warn` and a `fail` level, globally and per path.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::{Deserialize, Serialize};

use crate::config::LimitsConfig;
use crate::discover::absolute;

/// McCabe's original advice: keep functions at 10 or below.
pub const DEFAULT_WARN: u32 = 10;

/// Scores above `warn` are flagged; scores above `fail` fail the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level {
    pub warn: Option<u32>,
    pub fail: Option<u32>,
}

/// Ordered from best to worst, so the worst of several is their `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Excluded from the limits by a `dgf-ignore` comment or the baseline.
    Ignored,
    Ok,
    Warn,
    Fail,
}

impl Level {
    pub fn status(&self, complexity: u32) -> Status {
        if self.fail.is_some_and(|limit| complexity > limit) {
            Status::Fail
        } else if self.warn.is_some_and(|limit| complexity > limit) {
            Status::Warn
        } else {
            Status::Ok
        }
    }

    pub fn describe(&self) -> String {
        let part = |name, limit: Option<u32>| match limit {
            Some(limit) => format!("{name} above {limit}"),
            None => format!("no {name} limit"),
        };
        format!("{}, {}", part("warn", self.warn), part("fail", self.fail))
    }
}

struct Override {
    matcher: Gitignore,
    root: PathBuf,
    warn: Option<u32>,
    fail: Option<u32>,
}

impl Override {
    fn matches(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
            && self
                .matcher
                .matched_path_or_any_parents(path, false)
                .is_ignore()
    }
}

/// CRAP score limits, used when coverage is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CrapLimits {
    pub warn: Option<f64>,
    pub fail: Option<f64>,
}

/// Savoia's line: a CRAP score above 30 is "crappy" code.
pub const DEFAULT_CRAP_WARN: f64 = 30.0;

impl CrapLimits {
    pub fn status(&self, crap: f64) -> Status {
        if self.fail.is_some_and(|limit| crap > limit) {
            Status::Fail
        } else if self.warn.is_some_and(|limit| crap > limit) {
            Status::Warn
        } else {
            Status::Ok
        }
    }
}

pub struct Limits {
    pub global: Level,
    pub crap: CrapLimits,
    overrides: Vec<Override>,
}

impl Limits {
    /// `root` is the folder that override paths are relative to (where
    /// dgf.toml lives). `warn` and `fail` come from the command line and
    /// replace the global values from the file.
    pub fn new(
        config: &LimitsConfig,
        root: &Path,
        warn: Option<u32>,
        fail: Option<u32>,
    ) -> Result<Self, String> {
        let global = Level {
            warn: warn.or(config.warn).or(Some(DEFAULT_WARN)),
            fail: fail.or(config.fail),
        };
        let root = absolute(root);
        let mut overrides = Vec::new();
        for entry in &config.overrides {
            let mut builder = GitignoreBuilder::new(&root);
            for pattern in &entry.paths {
                builder
                    .add_line(None, pattern)
                    .map_err(|err| format!("invalid limits override path `{pattern}`: {err}"))?;
            }
            overrides.push(Override {
                matcher: builder.build().map_err(|err| err.to_string())?,
                root: root.clone(),
                warn: entry.warn,
                fail: entry.fail,
            });
        }
        let crap = CrapLimits {
            warn: config.crap_warn.or(Some(DEFAULT_CRAP_WARN)),
            fail: config.crap_fail,
        };
        Ok(Limits {
            global,
            crap,
            overrides,
        })
    }

    /// The limits for one file: the global ones, changed by every matching
    /// override in order, so a later override wins over an earlier one.
    pub fn for_file(&self, path: &Path) -> Level {
        let path = absolute(path);
        self.overrides
            .iter()
            .filter(|entry| entry.matches(&path))
            .fold(self.global, |level, entry| Level {
                warn: entry.warn.or(level.warn),
                fail: entry.fail.or(level.fail),
            })
    }

    pub fn override_count(&self) -> usize {
        self.overrides.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    fn root() -> PathBuf {
        std::env::temp_dir().join("dgf-limits-test")
    }

    fn limits(toml: &str) -> Limits {
        let config = config::parse(toml).unwrap();
        Limits::new(&config.limits, &root(), None, None).unwrap()
    }

    #[test]
    fn status_compares_strictly_above_each_level() {
        let level = Level {
            warn: Some(10),
            fail: Some(15),
        };
        assert_eq!(level.status(10), Status::Ok);
        assert_eq!(level.status(11), Status::Warn);
        assert_eq!(level.status(15), Status::Warn);
        assert_eq!(level.status(16), Status::Fail);
    }

    #[test]
    fn defaults_warn_above_10_and_never_fail() {
        let limits = limits("");
        assert_eq!(
            limits.global,
            Level {
                warn: Some(10),
                fail: None
            }
        );
        assert_eq!(limits.global.status(99), Status::Warn);
    }

    #[test]
    fn command_line_replaces_file_values() {
        let config = config::parse("[limits]\nwarn = 8\nfail = 12").unwrap();
        let limits = Limits::new(&config.limits, &root(), None, Some(20)).unwrap();
        assert_eq!(
            limits.global,
            Level {
                warn: Some(8),
                fail: Some(20)
            }
        );
    }

    #[test]
    fn override_applies_to_matching_folders_only() {
        let limits = limits("[limits]\nwarn = 10\nfail = 15\n[[limits.overrides]]\npaths = [\"legacy/\", \"**/tests/\"]\nwarn = 20");
        assert_eq!(
            limits.for_file(&root().join("legacy/old.rs")),
            Level {
                warn: Some(20),
                fail: Some(15)
            }
        );
        assert_eq!(
            limits.for_file(&root().join("crates/a/tests/t.rs")),
            Level {
                warn: Some(20),
                fail: Some(15)
            }
        );
        assert_eq!(limits.for_file(&root().join("src/main.rs")), limits.global);
    }

    #[test]
    fn later_override_wins() {
        let limits = limits(
            "[limits]\nfail = 15\n[[limits.overrides]]\npaths = [\"src/\"]\nfail = 30\n[[limits.overrides]]\npaths = [\"src/core/\"]\nfail = 12",
        );
        assert_eq!(limits.for_file(&root().join("src/ui/a.ts")).fail, Some(30));
        assert_eq!(
            limits.for_file(&root().join("src/core/b.ts")).fail,
            Some(12)
        );
    }

    #[test]
    fn files_outside_the_config_folder_use_global_limits() {
        let limits = limits("[[limits.overrides]]\npaths = [\"*\"]\nwarn = 50");
        let elsewhere = std::env::temp_dir().join("somewhere-else/a.rs");
        assert_eq!(limits.for_file(&elsewhere), limits.global);
    }
}
