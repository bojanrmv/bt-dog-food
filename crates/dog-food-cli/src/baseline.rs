//! `dgf baseline`: accept the functions that are over a limit today, so only
//! new problems (or accepted functions getting worse) fail later runs.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::changes::stable_name;
use crate::discover::absolute;
use crate::limits::Status;
use crate::report::Row;

pub const DEFAULT_FILE: &str = ".dgf/baseline.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Baseline {
    pub schema_version: u32,
    pub generated_at: u64,
    /// `[file relative to dgf.toml's folder, function name, accepted score]`
    pub functions: Vec<(String, String, u32)>,
}

impl Baseline {
    /// Every function that is over a limit in `rows`.
    pub fn from_rows(rows: &[Row], root: &Path, generated_at: u64) -> Self {
        let functions = rows
            .iter()
            .filter(|row| matches!(row.status, Status::Warn | Status::Fail))
            .map(|row| {
                (
                    relative(root, &row.file),
                    row.function.name.clone(),
                    row.function.complexity,
                )
            })
            .collect();
        Baseline {
            schema_version: 1,
            generated_at,
            functions,
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).expect("baseline serializes");
        std::fs::write(path, text + "\n")
            .map_err(|err| format!("could not write {}: {err}", path.display()))
    }

    /// `None` when there is no baseline file.
    pub fn load(path: &Path) -> Result<Option<Baseline>, String> {
        if !path.is_file() {
            return Ok(None);
        }
        let text =
            std::fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|err| format!("{}: not a dgf baseline ({err})", path.display()))
    }
}

/// Marks rows that are over a limit but no worse than their accepted score as
/// `Ignored`, and returns how many were accepted.
pub fn apply(rows: &mut [Row], baseline: &Baseline, root: &Path) -> usize {
    let accepted: HashMap<String, u32> = keyed(
        baseline
            .functions
            .iter()
            .map(|(file, name, score)| (file.clone(), name.as_str(), *score)),
    );
    let keys = row_keys(rows, root);
    let mut count = 0;
    for (row, key) in rows.iter_mut().zip(keys) {
        let Some(&score) = accepted.get(&key) else {
            continue;
        };
        if matches!(row.status, Status::Warn | Status::Fail) && row.function.complexity <= score {
            row.status = Status::Ignored;
            row.baseline = Some(score);
            count += 1;
        }
    }
    count
}

/// One key per row, in row order: file, stable name and a counter for
/// functions that share a name, counted in source order.
fn row_keys(rows: &[Row], root: &Path) -> Vec<String> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&a, &b| {
        (&rows[a].file, rows[a].function.start_line)
            .cmp(&(&rows[b].file, rows[b].function.start_line))
    });
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut keys = vec![String::new(); rows.len()];
    for index in order {
        let base = format!(
            "{}\u{0}{}",
            relative(root, &rows[index].file),
            stable_name(&rows[index].function.name)
        );
        let count = seen.entry(base.clone()).or_insert(0);
        *count += 1;
        keys[index] = format!("{base}#{count}");
    }
    keys
}

/// Keys for baseline entries, which are stored in source order already.
fn keyed<'a>(entries: impl Iterator<Item = (String, &'a str, u32)>) -> HashMap<String, u32> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    entries
        .map(|(file, name, score)| {
            let base = format!("{file}\u{0}{}", stable_name(name));
            let count = seen.entry(base.clone()).or_insert(0);
            *count += 1;
            (format!("{base}#{count}"), score)
        })
        .collect()
}

/// `file` relative to `root` with `/` separators, so the baseline works from
/// any folder and on any OS.
pub fn relative(root: &Path, file: &str) -> String {
    let path = absolute(Path::new(file));
    let root = absolute(root);
    path.strip_prefix(&root)
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::tests::{file, function, limits, run};

    #[test]
    fn accepted_functions_are_ignored_until_they_get_worse() {
        let limits = limits(5, 6);
        let before = file(vec![
            function("A.Big", 9, &[]),
            function("A.Mid", 6, &[]),
            function("A.Small", 2, &[]),
        ]);
        let root = Path::new(".");
        let baseline = Baseline::from_rows(&run(&before, &limits).rows, root, 1);
        assert_eq!(baseline.functions.len(), 2);

        let after = file(vec![
            function("A.Big", 9, &[]),
            function("A.Mid", 7, &[]),
            function("A.New", 8, &[]),
        ]);
        let mut rows = run(&after, &limits).rows;
        assert_eq!(apply(&mut rows, &baseline, root), 1);
        let status: Vec<(&str, Status)> = rows
            .iter()
            .map(|r| (r.function.name.as_str(), r.status))
            .collect();
        assert_eq!(
            status,
            [
                ("A.Big", Status::Ignored),
                ("A.New", Status::Fail),
                ("A.Mid", Status::Fail)
            ]
        );
        assert_eq!(rows[0].baseline, Some(9));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".dgf/baseline.json");
        assert!(Baseline::load(&path).unwrap().is_none());
        let baseline = Baseline {
            schema_version: 1,
            generated_at: 5,
            functions: vec![("src/a.rs".into(), "f".into(), 9)],
        };
        baseline.save(&path).unwrap();
        assert_eq!(
            Baseline::load(&path).unwrap().unwrap().functions,
            baseline.functions
        );
    }

    #[test]
    fn paths_are_relative_to_the_root() {
        let root = std::env::temp_dir();
        let file = root.join("src").join("a.rs");
        assert_eq!(relative(&root, &file.to_string_lossy()), "src/a.rs");
    }
}
