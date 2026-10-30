//! What a branch changed, from git: the changed lines of each file since
//! the merge base, uncommitted edits and new files included.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Changed line ranges (1-based, inclusive) by path relative to the
/// repository root.
pub struct Changes {
    pub root: PathBuf,
    pub base: String,
    /// The merge base, short.
    pub commit: String,
    /// The merge base, in full.
    pub fork: String,
    pub lines: HashMap<String, Vec<(usize, usize)>>,
}

impl Changes {
    /// Since where HEAD left `base` (`main` or `master` when `None`).
    pub fn since(dir: &Path, base: Option<&str>) -> Result<Changes, String> {
        let root = PathBuf::from(
            first_line(&git(dir, &["rev-parse", "--show-toplevel"])?)
                .ok_or("not inside a git repository")?,
        );
        let base = match base {
            Some(base) => base.to_string(),
            None => ["main", "master"]
                .into_iter()
                .find(|b| git(&root, &["rev-parse", "--verify", "--quiet", b]).is_ok())
                .ok_or("no `main` or `master` branch to compare with; pass --base")?
                .to_string(),
        };
        let fork = first_line(&git(&root, &["merge-base", &base, "HEAD"])?)
            .ok_or_else(|| format!("no common commit between `{base}` and HEAD"))?;
        let mut lines = parse_diff(&git(
            &root,
            &["diff", "-U0", "--no-color", "--no-ext-diff", &fork, "--"],
        )?);
        for path in git(&root, &["ls-files", "--others", "--exclude-standard"])?.lines() {
            if !path.is_empty() {
                lines.insert(path.to_string(), vec![(1, usize::MAX)]);
            }
        }
        let commit =
            first_line(&git(&root, &["rev-parse", "--short", &fork])?).unwrap_or(fork.clone());
        Ok(Changes {
            root,
            base,
            commit,
            fork,
            lines,
        })
    }

    /// Writes the merge base's files into `dir` (an empty folder), without
    /// touching the repository: a temporary index read from the commit and
    /// checked out there. Each restored C# project's package list
    /// (`obj/project.assets.json`) is copied along, so the C# engine binds
    /// the base as it binds the branch.
    pub fn export_base(&self, dir: &Path) -> Result<(), String> {
        let index = dir.join(".dgf-base-index");
        git_with_index(&self.root, &["read-tree", &self.fork], &index)?;
        let work_tree = format!("--work-tree={}", dir.display());
        git_with_index(&self.root, &[&work_tree, "checkout-index", "--all"], &index)?;
        let _ = std::fs::remove_file(&index);
        for assets in restored_projects(&self.root) {
            let Ok(relative) = assets.strip_prefix(&self.root) else {
                continue;
            };
            let to = dir.join(relative);
            if to
                .parent()
                .is_some_and(|p| std::fs::create_dir_all(p).is_ok())
            {
                let _ = std::fs::copy(&assets, to);
            }
        }
        Ok(())
    }

    /// Whether lines `start..=end` of `file` (relative to the root) changed.
    pub fn touches(&self, file: &str, start: usize, end: usize) -> bool {
        self.lines
            .get(file)
            .is_some_and(|ranges| ranges.iter().any(|&(from, to)| start <= to && from <= end))
    }
}

/// `git` with its index in `index` instead of the repository's.
fn git_with_index(dir: &Path, args: &[&str], index: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .output()
        .map_err(|err| format!("could not run git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Every `obj/project.assets.json` under `root`, not in build output,
/// packages or git's own folder.
fn restored_projects(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for dir in subfolders(&folder) {
            let assets = dir.join("project.assets.json");
            match dir.file_name().and_then(|n| n.to_str()) {
                Some("obj") if assets.is_file() => found.push(assets),
                Some("obj") => {}
                _ => folders.push(dir),
            }
        }
    }
    found
}

/// The folders in `folder`, without git's, packages' and build output's.
fn subfolders(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .filter(|p| {
            !matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some(".git" | "node_modules" | "bin" | "target")
            )
        })
        .collect()
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .output()
        .map_err(|err| format!("could not run git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
}

/// Changed line ranges of the new side of `git diff -U0` output.
fn parse_diff(diff: &str) -> HashMap<String, Vec<(usize, usize)>> {
    let mut found: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            current = path.strip_prefix("b/").map(str::to_string);
            continue;
        }
        let (Some(path), Some(hunk)) = (&current, line.strip_prefix("@@ ")) else {
            continue;
        };
        // `@@ -a,b +c,d @@`: the new side starts at c and has d lines.
        let Some(new) = hunk.split_whitespace().find(|p| p.starts_with('+')) else {
            continue;
        };
        let mut parts = new[1..].split(',');
        let start: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let count: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        if count > 0 {
            found
                .entry(path.clone())
                .or_default()
                .push((start, start + count - 1));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_give_the_changed_lines_of_the_new_side() {
        let diff = "diff --git a/src/A.cs b/src/A.cs\n--- a/src/A.cs\n+++ b/src/A.cs\n@@ -10,0 +11,3 @@ class A\n+x\n@@ -40 +43 @@\n-y\n+z\n@@ -50,2 +53,0 @@\n";
        let lines = parse_diff(diff);
        assert_eq!(lines["src/A.cs"], [(11, 13), (43, 43)]);
    }
}
