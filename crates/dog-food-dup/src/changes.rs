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
        let commit = first_line(&git(&root, &["rev-parse", "--short", &fork])?).unwrap_or(fork);
        Ok(Changes {
            root,
            base,
            commit,
            lines,
        })
    }

    /// Whether lines `start..=end` of `file` (relative to the root) changed.
    pub fn touches(&self, file: &str, start: usize, end: usize) -> bool {
        self.lines
            .get(file)
            .is_some_and(|ranges| ranges.iter().any(|&(from, to)| start <= to && from <= end))
    }
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
