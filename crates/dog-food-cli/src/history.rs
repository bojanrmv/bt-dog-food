//! Snapshots of past `dgf analyze --save-history` runs, and the overview
//! `dgf history` builds from them: trends over time and the biggest movers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::changes::stable_name;
use crate::limits::{Level, Status};
use crate::report::Run;

pub const DEFAULT_DIR: &str = ".dgf/history";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub files: usize,
    pub functions: usize,
    pub max_complexity: u32,
    pub median_complexity: u32,
    pub total_complexity: u64,
    pub ok: usize,
    pub warn: usize,
    pub fail: usize,
    /// Functions excused by `dgf-ignore` or the baseline (missing in older snapshots).
    #[serde(default)]
    pub ignored: usize,
    pub limits: Level,
}

/// One saved run: the numbers plus `[file, name, score]` for every function.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub schema_version: u32,
    pub generated_at: u64,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub summary: Summary,
    pub functions: Vec<(String, String, u32)>,
}

impl Snapshot {
    pub fn from_run(
        run: &Run,
        generated_at: u64,
        commit: Option<String>,
        branch: Option<String>,
    ) -> Self {
        let mut scores: Vec<u32> = run.rows.iter().map(|r| r.function.complexity).collect();
        scores.sort_unstable();
        let median = scores
            .get(scores.len().saturating_sub(1) / 2)
            .copied()
            .unwrap_or(0);
        Snapshot {
            schema_version: 1,
            generated_at,
            commit,
            branch,
            summary: Summary {
                files: run.files,
                functions: run.rows.len(),
                max_complexity: run.max_complexity(),
                median_complexity: median,
                total_complexity: scores.iter().map(|&s| u64::from(s)).sum(),
                ok: run.count(Status::Ok),
                warn: run.count(Status::Warn),
                fail: run.count(Status::Fail),
                ignored: run.count(Status::Ignored),
                limits: run.limits.global,
            },
            functions: run
                .rows
                .iter()
                .map(|r| {
                    (
                        r.file.clone(),
                        r.function.name.clone(),
                        r.function.complexity,
                    )
                })
                .collect(),
        }
    }

    /// `2026-10-22T082503_4442e7c.json`: sorts by time, says which commit.
    pub fn file_name(&self) -> String {
        let stamp = utc(self.generated_at)
            .replace([' ', ':'], "")
            .replacen('-', "", 2);
        let stamp = format!("{}T{}", &stamp[..8], &stamp[8..]);
        format!(
            "{stamp}_{}.json",
            self.commit.as_deref().unwrap_or("no-commit")
        )
    }
}

pub fn save(dir: &Path, snapshot: &Snapshot) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    let path = dir.join(snapshot.file_name());
    let text = serde_json::to_string(snapshot).expect("snapshot serializes");
    std::fs::write(&path, text)
        .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    Ok(path)
}

/// Every snapshot in `dir`, oldest first, plus a warning per unreadable file.
pub fn load(dir: &Path) -> Result<(Vec<Snapshot>, Vec<String>), String> {
    let entries = std::fs::read_dir(dir).map_err(|_| {
        format!(
            "no history in {}; save runs with `dgf analyze --save-history` first",
            dir.display()
        )
    })?;
    let mut snapshots = Vec::new();
    let mut warnings = Vec::new();
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.extension().is_some_and(|ext| ext == "json") {
            match read(&path) {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(err) => warnings.push(err),
            }
        }
    }
    snapshots.sort_by_key(|s| s.generated_at);
    Ok((snapshots, warnings))
}

fn read(path: &Path) -> Result<Snapshot, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|err| format!("{}: not a dgf snapshot ({err})", path.display()))
}

/// A run in the overview, without its per-function scores.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub generated_at: u64,
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub summary: Summary,
}

/// A function whose score changed between the first and the last run.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mover {
    pub file: String,
    pub name: String,
    pub before: Option<u32>,
    pub after: Option<u32>,
    pub delta: i64,
    pub status: Status,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    pub runs: Vec<RunSummary>,
    /// Biggest increases first, biggest decreases last.
    pub movers: Vec<Mover>,
    pub warnings: Vec<String>,
}

impl Overview {
    pub fn build(snapshots: Vec<Snapshot>, warnings: Vec<String>) -> Self {
        let movers = match (snapshots.first(), snapshots.last()) {
            (Some(first), Some(last)) if snapshots.len() > 1 => movers(first, last),
            _ => Vec::new(),
        };
        let runs = snapshots
            .into_iter()
            .map(|s| RunSummary {
                generated_at: s.generated_at,
                commit: s.commit,
                branch: s.branch,
                summary: s.summary,
            })
            .collect();
        Overview {
            runs,
            movers,
            warnings,
        }
    }
}

/// Functions whose score differs between two snapshots, matched by file and
/// stable name (see `changes::keyed`).
pub fn movers(first: &Snapshot, last: &Snapshot) -> Vec<Mover> {
    let old = scores(first);
    let new = scores(last);
    let limits = last.summary.limits;
    let mut movers: Vec<Mover> = new
        .iter()
        .map(|(key, (file, name, score))| {
            mover(file, name, old.get(key).map(|o| o.2), Some(*score), limits)
        })
        .chain(
            old.iter()
                .filter(|(key, _)| !new.contains_key(*key))
                .map(|(_, (file, name, score))| mover(file, name, Some(*score), None, limits)),
        )
        .filter(|m| m.delta != 0)
        .collect();
    movers.sort_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.name.cmp(&b.name)));
    movers
}

fn mover(file: &str, name: &str, before: Option<u32>, after: Option<u32>, limits: Level) -> Mover {
    Mover {
        file: file.to_string(),
        name: name.to_string(),
        before,
        after,
        delta: i64::from(after.unwrap_or(0)) - i64::from(before.unwrap_or(0)),
        status: after.map_or(Status::Ok, |score| limits.status(score)),
    }
}

type Entry<'a> = (&'a str, &'a str, u32);

fn scores(snapshot: &Snapshot) -> HashMap<String, Entry<'_>> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    snapshot
        .functions
        .iter()
        .map(|(file, name, score)| {
            let stable = format!("{file}\u{0}{}", stable_name(name));
            let count = seen.entry(stable.clone()).or_insert(0);
            *count += 1;
            (
                format!("{stable}#{count}"),
                (file.as_str(), name.as_str(), *score),
            )
        })
        .collect()
}

/// `2026-10-22 08:25:03` (UTC) for seconds since 1970.
pub fn utc(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Calendar date for days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(at: u64, functions: &[(&str, &str, u32)]) -> Snapshot {
        Snapshot {
            schema_version: 1,
            generated_at: at,
            commit: Some("abc1234".into()),
            branch: Some("main".into()),
            summary: Summary {
                files: 1,
                functions: functions.len(),
                max_complexity: functions.iter().map(|f| f.2).max().unwrap_or(0),
                median_complexity: 0,
                total_complexity: 0,
                ok: 0,
                warn: 0,
                fail: 0,
                ignored: 0,
                limits: Level {
                    warn: Some(5),
                    fail: Some(6),
                },
            },
            functions: functions
                .iter()
                .map(|(f, n, s)| (f.to_string(), n.to_string(), *s))
                .collect(),
        }
    }

    #[test]
    fn utc_formats_dates() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(utc(1_792_657_503), "2026-10-22 08:25:03");
    }

    #[test]
    fn file_names_sort_by_time() {
        assert_eq!(
            snapshot(1_792_657_503, &[]).file_name(),
            "20261022T082503_abc1234.json"
        );
    }

    #[test]
    fn movers_are_sorted_worst_first() {
        let first = snapshot(
            1,
            &[
                ("a.rs", "grow", 2),
                ("a.rs", "shrink", 9),
                ("a.rs", "same", 3),
                ("a.rs", "gone", 4),
            ],
        );
        let last = snapshot(
            2,
            &[
                ("a.rs", "grow", 8),
                ("a.rs", "shrink", 5),
                ("a.rs", "same", 3),
                ("b.rs", "added", 7),
            ],
        );
        let found: Vec<(String, i64, Status)> = movers(&first, &last)
            .into_iter()
            .map(|m| (m.name, m.delta, m.status))
            .collect();
        assert_eq!(
            found,
            [
                ("added".into(), 7, Status::Fail),
                ("grow".into(), 6, Status::Fail),
                ("gone".into(), -4, Status::Ok),
                ("shrink".into(), -4, Status::Ok),
            ]
        );
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &snapshot(200, &[("a.rs", "f", 3)])).unwrap();
        save(dir.path(), &snapshot(100, &[("a.rs", "f", 2)])).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();
        let (snapshots, warnings) = load(dir.path()).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            snapshots.iter().map(|s| s.generated_at).collect::<Vec<_>>(),
            [100, 200]
        );
        let overview = Overview::build(snapshots, warnings);
        assert_eq!(overview.runs.len(), 2);
        assert_eq!(overview.movers[0].delta, 1);
    }

    #[test]
    fn missing_history_folder_explains_what_to_do() {
        let err = load(Path::new("definitely/not/here")).unwrap_err();
        assert!(err.contains("--save-history"), "{err}");
    }
}
