//! What a mutation run of one language depends on, worked out from the
//! repository before anything is copied: the test projects (C#) or the test
//! package (TypeScript/JavaScript), the folders the sandbox needs, and
//! fingerprints of the build settings and of the test files, which decide
//! whether a cached result still holds.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::projects::{self, TestProject};
use super::sandbox::{self, Kind, SKIP};
use super::{fnv, Options};
use crate::config::SandboxMode;
use crate::discover::absolute;
use crate::mutate::Target;

pub struct Scope {
    pub kind: Kind,
    /// The repository, absolute.
    pub root: PathBuf,
    /// Folders the sandbox copies, absolute.
    pub roots: Vec<PathBuf>,
    /// C#: the test projects to run.
    pub tests: Vec<TestProject>,
    /// C#: test projects that use the mutated code but do not run.
    pub left_out: Vec<String>,
    /// Fingerprint of the build settings.
    pub build: u64,
    /// Fingerprint of every file of the chosen tests, and their filters.
    pub test_set: u64,
    /// File hashes by path relative to the root, read once.
    hashes: Mutex<HashMap<String, Option<u64>>>,
}

/// Files whose content changes how C# builds, wherever they sit above a project.
const DOTNET_SETTINGS: &[&str] = &[
    "directory.build.props",
    "directory.build.targets",
    "directory.packages.props",
    "global.json",
    "nuget.config",
];

/// Files whose content changes how TypeScript and JavaScript tests run.
const NODE_SETTINGS: &[&str] = &[
    "package.json",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "angular.json",
    "tsconfig",
    "vitest.config",
    "jest.config",
];

impl Scope {
    pub fn new(kind: Kind, targets: &[&Target], options: &Options) -> Result<Scope, String> {
        let root = absolute(&options.root);
        match kind {
            Kind::CSharp => Self::dotnet(root, targets, options),
            Kind::Node => Self::node(root, targets, options),
        }
    }

    fn dotnet(root: PathBuf, targets: &[&Target], options: &Options) -> Result<Scope, String> {
        let mutated: HashSet<PathBuf> = targets
            .iter()
            .filter_map(|t| sandbox::project_file(&absolute(&t.path)))
            .map(|p| absolute(&p))
            .collect();
        let tests = projects::select(&root, &mutated, &options.tests)?;
        let left_out = projects::left_out(&root, &mutated, &tests);
        let mut used: Vec<PathBuf> = tests.iter().map(|t| root.join(&t.path)).collect();
        used.extend(mutated);
        let closure = projects::closure(&used);
        let folders: Vec<PathBuf> = closure
            .iter()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect();
        let mut settings: Vec<PathBuf> = closure.iter().cloned().collect();
        settings.extend(above(&root, &folders, DOTNET_SETTINGS));
        let test_folders: Vec<PathBuf> = tests
            .iter()
            .filter_map(|t| root.join(&t.path).parent().map(Path::to_path_buf))
            .collect();
        let filters: Vec<String> = tests
            .iter()
            .map(|t| format!("{}={:?}", t.name(), t.filter))
            .collect();
        let test_set = fingerprint(&root, &files_under(&test_folders, |_| true), &filters);
        Ok(Scope {
            kind: Kind::CSharp,
            roots: sandbox_roots(&root, folders, options.sandbox),
            build: fingerprint(&root, &settings, &[]),
            test_set,
            tests,
            left_out,
            hashes: Mutex::default(),
            root,
        })
    }

    fn node(root: PathBuf, targets: &[&Target], options: &Options) -> Result<Scope, String> {
        let first = targets
            .first()
            .ok_or("no TypeScript or JavaScript mutants")?;
        let package = super::node::package_of(&absolute(&first.path), &root)
            .ok_or("no package.json with vitest or jest above the mutated files")?;
        let mut settings = files_under(std::slice::from_ref(&package), |p| {
            p.parent() == Some(package.as_path()) && is_setting(p, NODE_SETTINGS)
        });
        settings.extend(above(&root, std::slice::from_ref(&package), NODE_SETTINGS));
        let tests = files_under(std::slice::from_ref(&package), is_test_file);
        Ok(Scope {
            kind: Kind::Node,
            roots: sandbox_roots(&root, vec![package], options.sandbox),
            build: fingerprint(&root, &settings, &[]),
            test_set: fingerprint(&root, &tests, &[]),
            tests: Vec::new(),
            left_out: Vec::new(),
            hashes: Mutex::default(),
            root,
        })
    }

    /// A scope with given fingerprints, for tests of the cache.
    #[cfg(test)]
    pub fn fixed(kind: Kind, root: &Path, build: u64, test_set: u64) -> Scope {
        Scope {
            kind,
            root: absolute(root),
            roots: Vec::new(),
            tests: Vec::new(),
            left_out: Vec::new(),
            build,
            test_set,
            hashes: Mutex::default(),
        }
    }

    /// Hash of a file's content, by its path relative to the root.
    pub fn file_hash(&self, relative: &str) -> Option<u64> {
        let mut hashes = self.hashes.lock().expect("hash memo is not poisoned");
        *hashes.entry(relative.to_string()).or_insert_with(|| {
            std::fs::read(self.root.join(relative))
                .ok()
                .map(|bytes| fnv(&[&bytes]))
        })
    }
}

/// The folders the sandbox copies: the project folders, or the whole
/// repository with `sandbox = "full"`.
fn sandbox_roots(root: &Path, folders: Vec<PathBuf>, mode: SandboxMode) -> Vec<PathBuf> {
    match mode {
        SandboxMode::Full => vec![root.to_path_buf()],
        SandboxMode::Closure => {
            let unique: BTreeSet<PathBuf> = folders.into_iter().collect();
            unique.into_iter().collect()
        }
    }
}

/// Settings files in the folders above `folders`, up to the root.
fn above(root: &Path, folders: &[PathBuf], names: &[&str]) -> Vec<PathBuf> {
    let mut found = BTreeSet::new();
    for folder in folders {
        for dir in folder.ancestors().take_while(|d| d.starts_with(root)) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            found.extend(
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| is_setting(p, names)),
            );
        }
    }
    found.into_iter().collect()
}

fn is_setting(path: &Path, names: &[&str]) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    path.is_file() && names.iter().any(|n| name.starts_with(n))
}

/// Files under `folders` (skipping build output and dependencies) that `keep` accepts.
fn files_under(folders: &[PathBuf], keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut found = BTreeSet::new();
    for folder in folders {
        let walker = ignore::WalkBuilder::new(folder)
            .standard_filters(false)
            .filter_entry(|e| !SKIP.contains(&e.file_name().to_string_lossy().as_ref()))
            .build();
        found.extend(
            walker
                .flatten()
                .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                .map(|e| e.into_path())
                .filter(|p| keep(p)),
        );
    }
    found.into_iter().collect()
}

/// Test files of a TypeScript/JavaScript package: names or folders with
/// "test" or "spec".
fn is_test_file(path: &Path) -> bool {
    let text = path.to_string_lossy().to_lowercase();
    dog_food_core::Language::from_path(path).is_some()
        && ["test", "spec"].iter().any(|word| text.contains(word))
}

/// One hash over the paths (relative to the root) and contents of `files`,
/// plus `extra` text.
fn fingerprint(root: &Path, files: &[PathBuf], extra: &[String]) -> u64 {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for file in files {
        let relative = file.strip_prefix(root).unwrap_or(file);
        parts.push(relative.to_string_lossy().replace('\\', "/").into_bytes());
        parts.push(std::fs::read(file).unwrap_or_default());
    }
    parts.extend(extra.iter().map(|e| e.clone().into_bytes()));
    fnv(&parts.iter().map(Vec::as_slice).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_follow_content_and_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = absolute(dir.path());
        std::fs::write(root.join("a.cs"), "a").unwrap();
        let files = [root.join("a.cs")];
        let first = fingerprint(&root, &files, &[]);
        assert_eq!(first, fingerprint(&root, &files, &[]));
        std::fs::write(root.join("a.cs"), "b").unwrap();
        assert_ne!(first, fingerprint(&root, &files, &[]));
        assert_ne!(
            fingerprint(&root, &files, &[]),
            fingerprint(&root, &files, &["x".into()])
        );
    }

    #[test]
    fn settings_are_found_above_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = absolute(dir.path());
        std::fs::create_dir_all(root.join("src/Shop")).unwrap();
        std::fs::write(root.join("global.json"), "{}").unwrap();
        std::fs::write(root.join("src/Directory.Build.props"), "<Project/>").unwrap();
        std::fs::write(root.join("src/readme.md"), "x").unwrap();
        let found = above(&root, &[root.join("src/Shop")], DOTNET_SETTINGS);
        assert_eq!(
            found,
            [
                root.join("global.json"),
                root.join("src/Directory.Build.props")
            ]
        );
    }
}
