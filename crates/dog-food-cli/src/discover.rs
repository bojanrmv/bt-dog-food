//! Finds the source files to analyze.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use dog_food_core::Language;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// Generated code nobody maintains by hand; always skipped.
pub const DEFAULT_EXCLUDES: &[&str] = &["*.d.ts", "*.g.cs", "*.Designer.cs"];

/// Exclude patterns in .gitignore syntax, each set relative to its own folder.
#[derive(Default)]
pub struct Excludes {
    matchers: Vec<Gitignore>,
}

impl Excludes {
    /// Adds patterns that are relative to `root` (the folder of `dgf.toml`, or
    /// the current folder for `--exclude`).
    ///
    /// When `root` is reached through a link (a junction or symlink to the
    /// checkout), the patterns also hold under its real path, which is where
    /// git reports the changed files.
    pub fn add<S: AsRef<str>>(&mut self, root: &Path, patterns: &[S]) -> Result<(), String> {
        let shown = absolute(root);
        let mut roots = vec![shown.clone()];
        roots.extend(real(&shown).filter(|r| *r != shown));
        for root in roots {
            let mut builder = GitignoreBuilder::new(root);
            for pattern in patterns {
                builder.add_line(None, pattern.as_ref()).map_err(|err| {
                    format!("invalid exclude pattern `{}`: {err}", pattern.as_ref())
                })?;
            }
            self.matchers
                .push(builder.build().map_err(|err| err.to_string())?);
        }
        Ok(())
    }

    /// Whether `path`, or a folder it is in, matches an exclude pattern. A
    /// path outside every pattern folder is tried again by its real path, in
    /// case it was reached through another link.
    pub fn is_excluded(&self, path: &Path, is_dir: bool) -> bool {
        let path = absolute(path);
        if self.matches(&path, is_dir) {
            return true;
        }
        let inside = self.matchers.iter().any(|m| path.starts_with(m.path()));
        !inside && real(&path).is_some_and(|r| r != path && self.matches(&r, is_dir))
    }

    fn matches(&self, path: &Path, is_dir: bool) -> bool {
        self.matchers
            .iter()
            .any(|matcher| matches(matcher, path, is_dir))
    }
}

/// The path with every link resolved, without Windows' `\\?\` prefix.
pub(crate) fn real(path: &Path) -> Option<PathBuf> {
    let full = std::fs::canonicalize(path).ok()?;
    let text = full.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => Some(PathBuf::from(rest)),
        _ => Some(full),
    }
}

fn matches(matcher: &Gitignore, path: &Path, is_dir: bool) -> bool {
    // The parent-aware check only works (and panics otherwise) inside the matcher's folder.
    if path.starts_with(matcher.path()) {
        matcher
            .matched_path_or_any_parents(path, is_dir)
            .is_ignore()
    } else {
        matcher.matched(path, is_dir).is_ignore()
    }
}

/// All supported source files under `paths`, honoring .gitignore (even outside
/// a git repo) and `excludes`. Excluded folders are not entered at all.
pub fn discover(paths: &[PathBuf], excludes: Excludes) -> Vec<PathBuf> {
    let excludes = Arc::new(excludes);
    let mut files = Vec::new();
    for root in paths {
        let filter = Arc::clone(&excludes);
        let walker = ignore::WalkBuilder::new(root)
            .require_git(false)
            .filter_entry(move |entry| {
                let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
                !filter.is_excluded(entry.path(), is_dir)
            })
            .build();
        for entry in walker.flatten() {
            let path = entry.path();
            if path.is_file()
                && Language::from_path(path).is_some()
                && !excludes.is_excluded(path, false)
            {
                files.push(path.to_path_buf());
            }
        }
    }
    files.sort();
    files
}

/// The absolute path with `.` and `..` resolved by text, as Windows and
/// MSBuild do; on Unix `std::path::absolute` keeps `App.Tests/../App`.
pub(crate) fn absolute(path: &Path) -> PathBuf {
    let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut clean = PathBuf::new();
    for part in full.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other),
        }
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_hold_through_a_link_to_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let real_root = dir.path().join("repo");
        std::fs::create_dir_all(real_root.join("src/Migrations")).unwrap();
        std::fs::write(real_root.join("src/Migrations/M.cs"), "").unwrap();
        std::fs::write(real_root.join("src/Keep.cs"), "").unwrap();
        let link = dir.path().join("link");
        // A junction, the link Windows makes without extra rights.
        #[cfg(windows)]
        let linked = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&real_root)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .ok_or(());
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&real_root, &link);
        if linked.is_err() {
            tellln!("skipped: this machine cannot create symlinks");
            return;
        }
        // dgf.toml found through the link, files named by their real path (as git does).
        let mut excludes = Excludes::default();
        excludes.add(&link, &["**/Migrations/"]).unwrap();
        let real_root = real(&real_root).unwrap();
        assert!(excludes.is_excluded(&real_root.join("src/Migrations/M.cs"), false));
        assert!(!excludes.is_excluded(&real_root.join("src/Keep.cs"), false));
        assert!(excludes.is_excluded(&link.join("src/Migrations/M.cs"), false));
        // And the other way round.
        let mut excludes = Excludes::default();
        excludes.add(&real_root, &["**/Migrations/"]).unwrap();
        assert!(excludes.is_excluded(&link.join("src/Migrations/M.cs"), false));
        assert!(!excludes.is_excluded(&link.join("src/Keep.cs"), false));
    }

    /// A temp folder with the given files (all empty).
    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        dir
    }

    fn found(dir: &tempfile::TempDir, excludes: Excludes) -> Vec<String> {
        discover(&[dir.path().to_path_buf()], excludes)
            .iter()
            .map(|path| {
                path.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect()
    }

    fn defaults(root: &Path) -> Excludes {
        let mut excludes = Excludes::default();
        excludes.add(root, DEFAULT_EXCLUDES).unwrap();
        excludes
    }

    #[test]
    fn finds_only_supported_languages() {
        let dir = tree(&["a.rs", "b.ts", "c.tsx", "e.cs", "notes.md", "d.py"]);
        assert_eq!(
            found(&dir, Excludes::default()),
            ["a.rs", "b.ts", "c.tsx", "e.cs"]
        );
    }

    #[test]
    fn skips_generated_files_by_default() {
        let dir = tree(&[
            "src/app.ts",
            "src/types.d.ts",
            "lib/index.d.ts",
            "Api/Client.g.cs",
            "Forms/Main.Designer.cs",
            "Forms/Main.cs",
        ]);
        assert_eq!(
            found(&dir, defaults(dir.path())),
            ["Forms/Main.cs", "src/app.ts"]
        );
    }

    #[test]
    fn excludes_several_folders_and_globs() {
        let dir = tree(&[
            "src/main.rs",
            "tests/fixtures/x.rs",
            "vendor/lib.ts",
            "src/generated/api.ts",
            "src/a.spec.ts",
        ]);
        let mut excludes = Excludes::default();
        excludes
            .add(
                dir.path(),
                &["tests/fixtures/", "vendor/", "**/generated/", "*.spec.ts"],
            )
            .unwrap();
        assert_eq!(found(&dir, excludes), ["src/main.rs"]);
    }

    #[test]
    fn files_inside_an_excluded_folder_are_excluded() {
        let dir = tree(&[]);
        let mut excludes = Excludes::default();
        excludes
            .add(dir.path(), &["crates/*/tests/fixtures/"])
            .unwrap();
        let fixture = dir.path().join("crates/core/tests/fixtures/csharp/A.cs");
        assert!(excludes.is_excluded(&fixture, false));
        assert!(!excludes.is_excluded(&dir.path().join("crates/core/src/a.rs"), false));
        assert!(!excludes.is_excluded(Path::new("C:/elsewhere/tests/fixtures/x.rs"), false));
    }

    #[test]
    fn negated_pattern_brings_a_file_back() {
        let dir = tree(&["vendor/lib.ts", "vendor/ours.ts"]);
        let mut excludes = Excludes::default();
        excludes
            .add(dir.path(), &["vendor/*", "!vendor/ours.ts"])
            .unwrap();
        assert_eq!(found(&dir, excludes), ["vendor/ours.ts"]);
    }

    #[test]
    fn respects_gitignore_without_a_git_repo() {
        let dir = tree(&["src/main.rs", "target/gen.rs"]);
        std::fs::write(dir.path().join(".gitignore"), "/target\n").unwrap();
        assert_eq!(found(&dir, Excludes::default()), ["src/main.rs"]);
    }
}
