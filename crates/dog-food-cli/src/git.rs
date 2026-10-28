//! The few git commands dgf needs, run through the `git` executable, and a
//! parser for the changed line ranges in `git diff -U0` output.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One changed file: its path now, its path in the base commit (`None` when
/// the file is new) and the changed line ranges in the new version.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub ranges: Vec<(usize, usize)>,
}

impl FileChange {
    /// A file git does not track yet: every line is new.
    fn added(path: &str) -> Self {
        FileChange {
            path: path.to_string(),
            old_path: None,
            ranges: vec![(1, usize::MAX)],
        }
    }

    /// Whether lines `start..=end` overlap a changed range.
    pub fn touches(&self, start: usize, end: usize) -> bool {
        self.ranges
            .iter()
            .any(|&(from, to)| start <= to && from <= end)
    }
}

fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .output()
        .map_err(|err| format!("could not run git: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`git {}` failed: {}",
            args.join(" "),
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn first_line(text: String) -> Option<String> {
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// The top folder of the repository that contains `dir`.
pub fn root(dir: &Path) -> Result<PathBuf, String> {
    let top = run(dir, &["rev-parse", "--show-toplevel"]).map(first_line)?;
    top.map(PathBuf::from)
        .ok_or_else(|| "not inside a git repository".to_string())
}

/// The repository behind `dir`, the same for every worktree of it: the main
/// checkout's folder (or the bare repository).
pub fn repository(dir: &Path) -> Option<PathBuf> {
    let common = run(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()
    .and_then(first_line)?;
    let common = PathBuf::from(common);
    if common.file_name().is_some_and(|name| name == ".git") {
        return common.parent().map(Path::to_path_buf);
    }
    Some(common)
}

pub fn exists(root: &Path, rev: &str) -> bool {
    run(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .is_ok()
}

/// `main`, or `master` when there is no `main`.
pub fn default_base(root: &Path) -> Option<String> {
    ["main", "master"]
        .into_iter()
        .find(|branch| exists(root, branch))
        .map(str::to_string)
}

/// The commit where the current branch left `base`, and a note when it is
/// not where the local `base` says.
///
/// A local `base` branch can lag behind its remote copy. When the current
/// branch left the remote copy later, that later commit is used: otherwise
/// everything merged into the remote since the local branch was updated would
/// count as this branch's changes.
pub fn merge_base(root: &Path, base: &str) -> Result<(String, Option<String>), String> {
    if !exists(root, base) {
        return Err(format!(
            "base `{base}` not found; pass another one with --base"
        ));
    }
    let local = fork_point(root, base)?;
    let Some(remote) = remote_copy(root, base) else {
        return Ok((local, None));
    };
    let Ok(later) = fork_point(root, &remote) else {
        return Ok((local, None));
    };
    if later == local || !is_ancestor(root, &local, &later) {
        return Ok((local, None));
    }
    let behind = run(root, &["rev-list", "--count", &format!("{local}..{later}")])
        .ok()
        .and_then(first_line)
        .unwrap_or_else(|| "some".into());
    let note = format!(
        "`{base}` is behind `{remote}`: this branch left `{remote}` at {}, {behind} commits after where it left `{base}` ({}); comparing with {} (update `{base}` or pass --base to choose)",
        short(root, &later),
        short(root, &local),
        short(root, &later),
    );
    Ok((later, Some(note)))
}

fn fork_point(root: &Path, base: &str) -> Result<String, String> {
    run(root, &["merge-base", base, "HEAD"])
        .map(first_line)?
        .ok_or_else(|| format!("no common commit between `{base}` and HEAD"))
}

/// The remote branch a local branch tracks, else `origin/<branch>`; `None`
/// for a remote branch, a tag or a commit.
fn remote_copy(root: &Path, base: &str) -> Option<String> {
    if !exists(root, &format!("refs/heads/{base}")) {
        return None;
    }
    run(
        root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            &format!("{base}@{{upstream}}"),
        ],
    )
    .ok()
    .and_then(first_line)
    .or_else(|| {
        let origin = format!("origin/{base}");
        exists(root, &format!("refs/remotes/{origin}")).then_some(origin)
    })
}

fn is_ancestor(root: &Path, ancestor: &str, of: &str) -> bool {
    run(root, &["merge-base", "--is-ancestor", ancestor, of]).is_ok()
}

/// How many commits `base` has that HEAD does not.
pub fn behind(root: &Path, base: &str) -> Option<usize> {
    run(root, &["rev-list", "--count", &format!("HEAD..{base}")])
        .ok()
        .and_then(first_line)?
        .parse()
        .ok()
}

/// Short commit id and branch name of HEAD, when there are any.
pub fn head(root: &Path) -> (Option<String>, Option<String>) {
    let commit = run(root, &["rev-parse", "--short", "HEAD"])
        .ok()
        .and_then(first_line);
    let branch = run(root, &["branch", "--show-current"])
        .ok()
        .and_then(first_line);
    (commit, branch)
}

pub fn short(root: &Path, commit: &str) -> String {
    run(root, &["rev-parse", "--short", commit])
        .ok()
        .and_then(first_line)
        .unwrap_or_else(|| commit.chars().take(7).collect())
}

/// A file's content at `commit`, or `None` if it did not exist there.
pub fn show(root: &Path, commit: &str, path: &str) -> Option<String> {
    run(root, &["show", &format!("{commit}:{path}")]).ok()
}

/// Files that differ between `commit` and the working tree (committed and
/// uncommitted edits) plus untracked files, or with `staged` only what is
/// staged for the next commit. Deleted files are left out.
pub fn changed_files(root: &Path, commit: &str, staged: bool) -> Result<Vec<FileChange>, String> {
    Ok(own_changes(root, commit, staged)?.0)
}

/// The changes since `commit` (see `changed_files`) without the files that
/// only merge commits on the way changed (a branch that merged its base
/// after `commit`), and those files' paths.
pub fn own_changes(
    root: &Path,
    commit: &str,
    staged: bool,
) -> Result<(Vec<FileChange>, Vec<String>), String> {
    let files = all_changes(root, commit, staged)?;
    let merged = merged_in(root, commit, &files);
    let (dropped, own): (Vec<FileChange>, Vec<FileChange>) =
        files.into_iter().partition(|f| merged.contains(&f.path));
    Ok((own, dropped.into_iter().map(|f| f.path).collect()))
}

/// Files of `files` that only merges changed between `base` and HEAD: on
/// the first-parent chain from HEAD back to `base`, a merge changed them and
/// no commit of the branch did, and the working tree leaves them as in HEAD.
fn merged_in(root: &Path, base: &str, files: &[FileChange]) -> HashSet<String> {
    let merges: Vec<String> = run(
        root,
        &[
            "rev-list",
            "--first-parent",
            "--merges",
            &format!("{base}..HEAD"),
        ],
    )
    .map(|text| text.lines().map(str::to_string).collect())
    .unwrap_or_default();
    if merges.is_empty() {
        return HashSet::new();
    }
    let dirty: HashSet<String> = run(root, &["diff", "--name-only", "HEAD"])
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let mut trees: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut blob = |rev: &str, path: &str| -> Option<String> {
        trees
            .entry(rev.to_string())
            .or_insert_with(|| tree(root, rev))
            .get(path)
            .cloned()
    };
    files
        .iter()
        .filter(|f| !dirty.contains(&f.path))
        .filter(|f| {
            let mut current = "HEAD".to_string();
            let mut merged = false;
            for merge in &merges {
                if blob(&current, &f.path) != blob(merge, &f.path) {
                    return false;
                }
                let first = format!("{merge}^1");
                merged |= blob(merge, &f.path) != blob(&first, &f.path);
                current = first;
            }
            merged && blob(&current, &f.path) == blob(base, &f.path)
        })
        .map(|f| f.path.clone())
        .collect()
}

/// Path → blob id of every file in `rev`.
fn tree(root: &Path, rev: &str) -> HashMap<String, String> {
    run(root, &["ls-tree", "-r", "--full-tree", rev])
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            let id = meta.split_whitespace().nth(2)?;
            Some((path.to_string(), id.to_string()))
        })
        .collect()
}

fn all_changes(root: &Path, commit: &str, staged: bool) -> Result<Vec<FileChange>, String> {
    let mut args = vec!["diff", "-U0", "--no-color", "--no-ext-diff", "-M"];
    if staged {
        args.push("--cached");
    }
    args.extend([commit, "--"]);
    let mut files = parse_diff(&run(root, &args)?);
    if !staged {
        let untracked = run(root, &["ls-files", "--others", "--exclude-standard"])?;
        files.extend(
            untracked
                .lines()
                .filter(|line| !line.is_empty())
                .map(FileChange::added),
        );
    }
    Ok(files)
}

/// A file's content as staged for the next commit.
pub fn staged(root: &Path, path: &str) -> Option<String> {
    run(root, &["show", &format!(":{path}")]).ok()
}

/// Where git keeps `name` (e.g. `hooks/pre-commit`), honoring core.hooksPath.
pub fn git_path(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = run(root, &["rev-parse", "--git-path", name]).map(first_line)?;
    let path = PathBuf::from(path.ok_or_else(|| format!("git has no path for {name}"))?);
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

/// Reads file paths and changed line ranges from `git diff -U0` output.
pub fn parse_diff(text: &str) -> Vec<FileChange> {
    let mut files: Vec<FileChange> = Vec::new();
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            files.push(FileChange::default());
        } else if let Some(file) = files.last_mut() {
            read_line(file, line);
        }
    }
    // Deleted files and pure renames have no new path or no changed lines.
    files.retain(|file| !file.path.is_empty() && !file.ranges.is_empty());
    files
}

fn read_line(file: &mut FileChange, line: &str) {
    // Once the hunks start, a line like "--- a/x" is file content, not a header.
    if !file.ranges.is_empty() || line.starts_with("@@ ") {
        file.ranges
            .extend(line.strip_prefix("@@ ").and_then(hunk_range));
    } else if let Some(path) = line.strip_prefix("--- a/") {
        file.old_path = Some(path.to_string());
    } else if let Some(path) = line.strip_prefix("+++ b/") {
        file.path = path.to_string();
    }
}

/// The new-side lines of a hunk header such as `-12,3 +15,4 @@`. A pure
/// deletion (`+15,0`) touches the lines on either side of the gap.
fn hunk_range(header: &str) -> Option<(usize, usize)> {
    let new_side = header.split_whitespace().nth(1)?.strip_prefix('+')?;
    let (start, count) = match new_side.split_once(',') {
        Some((start, count)) => (start.parse::<usize>().ok()?, count.parse::<usize>().ok()?),
        None => (new_side.parse::<usize>().ok()?, 1),
    };
    Some(match count {
        0 => (start.max(1), start + 1),
        _ => (start, start + count - 1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/Cart.cs b/src/Cart.cs
index 1111111..2222222 100644
--- a/src/Cart.cs
+++ b/src/Cart.cs
@@ -10,0 +11,3 @@ public class Cart
+    if (x) {
+    }
+    return 1;
@@ -40 +43 @@ public class Cart
--- a/not/a/header
diff --git a/src/New.ts b/src/New.ts
new file mode 100644
--- /dev/null
+++ b/src/New.ts
@@ -0,0 +1,5 @@
+export function f() {}
diff --git a/src/Gone.rs b/src/Gone.rs
deleted file mode 100644
--- a/src/Gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-fn gone() {}
diff --git a/old/Name.cs b/new/Name.cs
similarity index 90%
rename from old/Name.cs
rename to new/Name.cs
--- a/old/Name.cs
+++ b/new/Name.cs
@@ -3,2 +3,0 @@
";

    #[test]
    fn parses_paths_and_new_side_ranges() {
        let files = parse_diff(DIFF);
        assert_eq!(files.len(), 3, "{files:?}");
        assert_eq!(files[0].path, "src/Cart.cs");
        assert_eq!(files[0].old_path.as_deref(), Some("src/Cart.cs"));
        assert_eq!(files[0].ranges, [(11, 13), (43, 43)]);
    }

    #[test]
    fn new_files_have_no_old_path() {
        let files = parse_diff(DIFF);
        assert_eq!(files[1].path, "src/New.ts");
        assert_eq!(files[1].old_path, None);
        assert_eq!(files[1].ranges, [(1, 5)]);
    }

    #[test]
    fn deleted_files_are_skipped_and_renames_keep_the_old_path() {
        let files = parse_diff(DIFF);
        assert!(files.iter().all(|f| f.path != "src/Gone.rs"));
        assert_eq!(files[2].path, "new/Name.cs");
        assert_eq!(files[2].old_path.as_deref(), Some("old/Name.cs"));
        // A pure deletion after line 3 touches lines 3 and 4.
        assert_eq!(files[2].ranges, [(3, 4)]);
    }

    #[test]
    fn touches_checks_overlap() {
        let file = FileChange {
            path: "a".into(),
            old_path: None,
            ranges: vec![(10, 12)],
        };
        assert!(file.touches(1, 10));
        assert!(file.touches(12, 30));
        assert!(!file.touches(13, 30));
        assert!(FileChange::added("b").touches(500, 900));
    }

    #[test]
    fn files_that_only_a_merge_brought_in_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            run(
                root,
                &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
            )
            .unwrap()
        };
        let commit = |name: &str, text: &str| {
            std::fs::write(root.join(name), text).unwrap();
            git(&["add", name]);
            git(&["commit", "-q", "-m", name]);
        };
        git(&["init", "-q", "-b", "main"]);
        commit("shared.cs", "1");
        let base = run(root, &["rev-parse", "HEAD"])
            .map(first_line)
            .unwrap()
            .unwrap();
        git(&["checkout", "-q", "-b", "feature"]);
        commit("own.cs", "mine");
        git(&["checkout", "-q", "main"]);
        commit("theirs.cs", "other slug");
        commit("shared.cs", "2");
        git(&["checkout", "-q", "feature"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "merge",
            "-q",
            "--no-edit",
            "main",
        ]);
        commit("shared.cs", "2 and mine");
        commit("after.cs", "mine too");

        let (own, dropped) = own_changes(root, &base, false).unwrap();
        let mut kept: Vec<&str> = own.iter().map(|f| f.path.as_str()).collect();
        kept.sort();
        assert_eq!(kept, ["after.cs", "own.cs", "shared.cs"]);
        assert_eq!(dropped, ["theirs.cs"]);
        // A working-tree edit makes the file the branch's own.
        std::fs::write(root.join("theirs.cs"), "edited here").unwrap();
        assert!(own_changes(root, &base, false).unwrap().1.is_empty());
    }

    #[test]
    fn a_local_base_behind_its_remote_copy_uses_the_later_fork_point() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            run(
                root,
                &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
            )
            .unwrap()
        };
        let commit = |name: &str| {
            std::fs::write(root.join(name), name).unwrap();
            git(&["add", name]);
            git(&["commit", "-q", "-m", name]);
        };
        git(&["init", "-q", "-b", "main"]);
        commit("a");
        let old = run(root, &["rev-parse", "HEAD"])
            .map(first_line)
            .unwrap()
            .unwrap();
        commit("b");
        // origin/main has b; local main is reset to a, behind it.
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let later = run(root, &["rev-parse", "HEAD"])
            .map(first_line)
            .unwrap()
            .unwrap();
        git(&["checkout", "-q", "-b", "feature"]);
        commit("c");
        git(&["branch", "-f", "main", &old]);

        let (point, note) = merge_base(root, "main").unwrap();
        assert_eq!(point, later);
        assert!(note.unwrap().contains("`main` is behind `origin/main`"));
        // The remote copy itself, or a base that is up to date, needs no note.
        assert_eq!(
            merge_base(root, "origin/main").unwrap(),
            (later.clone(), None)
        );
        git(&["branch", "-f", "main", &later]);
        assert_eq!(merge_base(root, "main").unwrap(), (later, None));
    }
}
