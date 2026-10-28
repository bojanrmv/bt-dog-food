//! The mutation cache: one result per mutant, reused while everything that
//! decided it is unchanged. A key never names a branch or a folder outside
//! the repository, so one cache folder can serve every clone and worktree.
//!
//! A result holds while the function's text, the mutant and the build
//! settings (project files, Directory.Build.props, global.json, package.json
//! and the like) are the same, and:
//! - killed: the files of the tests that killed it are unchanged;
//! - survived, no tests, timeout: no test file of the chosen tests changed,
//!   since any new or changed test might kill it;
//! - did not compile: nothing more.
//!
//! It does not notice a change in other
//! production code that makes a killing test miss the mutant; `--no-cache`
//! tests everything again.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::scope::Scope;
use super::Outcome;
use crate::changes::stable_name;
use crate::discover::absolute;
use crate::mutate::Target;

/// The cache folder, relative to dgf.toml (or the repository) unless absolute.
pub const DEFAULT_DIR: &str = ".dgf/mutation-cache";
const FILE: &str = "mutants.json";
const LOCK: &str = "mutants.lock";
const SCHEMA: u32 = 5;
/// Entries nobody used for this long are dropped when the cache is saved.
const KEEP_DAYS: u64 = 30;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cache {
    schema_version: u32,
    functions: HashMap<String, Entry>,
    /// Phase timings of the last run that tested a set of functions, by `run_key`.
    #[serde(default)]
    runs: HashMap<String, Timings>,
    #[serde(skip)]
    dir: PathBuf,
    /// Timings this run recorded: saved over what is on disk.
    #[serde(skip)]
    new_runs: HashMap<String, Timings>,
    /// Entries this run read or wrote: saved over what is on disk.
    #[serde(skip)]
    touched: HashSet<String>,
    /// This run tests survivors again (`--rerun-survivors`).
    #[serde(skip)]
    pub rerun: bool,
}

/// How long the phases of a run took, so a later run that takes every
/// result from the cache can still report them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Timings {
    pub prepare_ms: u64,
    pub build_ms: u64,
    pub baseline_ms: u64,
    pub mutants_ms: u64,
    pub test_runs: usize,
    /// Seconds since 1970 when the run ended.
    pub at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    /// Day (since 1970) the entry was last used.
    used: u64,
    mutants: Vec<CachedMutant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedMutant {
    /// Byte offset from the function's first line.
    offset: usize,
    mutator: String,
    replacement: String,
    outcome: Outcome,
    /// Fingerprint of the build settings.
    build: u64,
    /// Killed: the files of the tests that killed it, with their hashes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    killed_by: Vec<TestFile>,
    /// Fingerprint of every test file of the chosen tests, when the result
    /// depends on all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tests: Option<u64>,
    /// The files of the tests that reach the mutant: a change to one of
    /// them puts the function back in `dgf mutate --changes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reached: Vec<String>,
    /// Up to three tests that reach the mutant ("name (file)"), so a cached
    /// survivor still says where a new assertion goes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reached_tests: Vec<String>,
    /// A survivor that `--rerun-survivors` tested again against these tests:
    /// testing it once more would say the same.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    rechecked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TestFile {
    path: String,
    hash: u64,
}

impl CachedMutant {
    fn holds(&self, scope: &Scope) -> bool {
        if self.build != scope.build {
            return false;
        }
        if self.outcome == Outcome::CompileError {
            return true;
        }
        if !self.killed_by.is_empty() {
            return self
                .killed_by
                .iter()
                .all(|f| scope.file_hash(&f.path) == Some(f.hash));
        }
        self.tests == Some(scope.test_set)
    }
}

/// What a run learned about the mutants it killed: the repository files of
/// the tests that killed each one (empty when a test's file is unknown).
pub type KilledBy = HashMap<usize, Vec<String>>;

impl Cache {
    /// The cache in `dir`; empty when there is none or it has another format.
    pub fn load(dir: &Path) -> Cache {
        let saved = locked(dir, false, || read(dir)).ok().flatten();
        let mut cache = saved
            .filter(|c| c.schema_version == SCHEMA)
            .unwrap_or_default();
        cache.dir = dir.to_path_buf();
        cache
    }

    /// Outcomes that still hold for the mutants of `targets`.
    pub fn lookup(&mut self, targets: &[&Target], scope: &Scope) -> HashMap<usize, Outcome> {
        self.hits(targets, scope)
            .into_iter()
            .map(|(id, c)| (id, c.outcome))
            .collect()
    }

    /// The tests recorded as reaching each mutant whose outcome still holds.
    pub fn reached_tests(
        &mut self,
        targets: &[&Target],
        scope: &Scope,
    ) -> HashMap<usize, Vec<String>> {
        self.hits(targets, scope)
            .into_iter()
            .filter(|(_, c)| !c.reached_tests.is_empty())
            .map(|(id, c)| (id, c.reached_tests))
            .collect()
    }

    /// Survivors whose result still holds and that `--rerun-survivors`
    /// already tested again against these tests.
    pub fn rechecked(&mut self, targets: &[&Target], scope: &Scope) -> HashSet<usize> {
        self.hits(targets, scope)
            .into_iter()
            .filter(|(_, c)| c.rechecked)
            .map(|(id, _)| id)
            .collect()
    }

    /// The cached results that still hold, by mutant id.
    fn hits(&mut self, targets: &[&Target], scope: &Scope) -> HashMap<usize, CachedMutant> {
        let mut found = HashMap::new();
        for target in targets {
            let Some((key, start)) = identify(target, &scope.root) else {
                continue;
            };
            let Some(entry) = self.functions.get(&key) else {
                continue;
            };
            for mutant in &target.mutants {
                let cached = entry.mutants.iter().find(|c| {
                    c.offset == mutant.start_byte - start
                        && c.mutator == mutant.mutator
                        && c.replacement == mutant.replacement
                });
                found.extend(
                    cached
                        .filter(|c| c.holds(scope))
                        .map(|c| (mutant.id, c.clone())),
                );
            }
            self.touched.insert(key);
        }
        found
    }

    /// Remembers every outcome of `targets` worth keeping.
    pub fn store(
        &mut self,
        targets: &[&Target],
        scope: &Scope,
        outcomes: &HashMap<usize, Outcome>,
        killed_by: &KilledBy,
        reached: &HashMap<usize, BTreeSet<String>>,
        reached_tests: &HashMap<usize, Vec<String>>,
    ) {
        for target in targets {
            let Some((key, start)) = identify(target, &scope.root) else {
                continue;
            };
            let fresh: Vec<CachedMutant> = target
                .mutants
                .iter()
                .filter_map(|m| {
                    let outcome = *outcomes.get(&m.id).filter(|o| o.is_cached())?;
                    let mut entry = cached(
                        m.start_byte - start,
                        m,
                        outcome,
                        scope,
                        killed_by.get(&m.id),
                    );
                    entry.reached = reached
                        .get(&m.id)
                        .map(|files| files.iter().cloned().collect())
                        .unwrap_or_default();
                    entry.reached_tests = reached_tests.get(&m.id).cloned().unwrap_or_default();
                    entry.rechecked = self.rerun && outcome.undetected();
                    Some(entry)
                })
                .collect();
            let entry = self.functions.entry(key.clone()).or_default();
            merge(&mut entry.mutants, fresh);
            entry.used = today();
            self.touched.insert(key);
        }
    }

    /// Writes the entries this run used into the cache file, keeping what
    /// other runs wrote meanwhile. Other dgf processes wait for the lock.
    pub fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|err| format!("{}: {err}", self.dir.display()))?;
        locked(&self.dir, true, || {
            let mut on_disk = read(&self.dir)?
                .filter(|c| c.schema_version == SCHEMA)
                .unwrap_or_default();
            for key in &self.touched {
                let Some(ours) = self.functions.get(key) else {
                    continue;
                };
                let entry = on_disk.functions.entry(key.clone()).or_default();
                merge(&mut entry.mutants, ours.mutants.clone());
                entry.used = today();
            }
            on_disk.runs.extend(self.new_runs.clone());
            let oldest = today().saturating_sub(KEEP_DAYS);
            on_disk.functions.retain(|_, e| e.used >= oldest);
            on_disk.runs.retain(|_, t| t.at / 86_400 >= oldest);
            on_disk.schema_version = SCHEMA;
            write(&self.dir, &on_disk)
        })
    }

    /// The timings of the last run that tested exactly the functions of `key`.
    pub fn timings(&self, key: &str) -> Option<Timings> {
        self.new_runs
            .get(key)
            .or_else(|| self.runs.get(key))
            .copied()
    }

    pub fn remember_timings(&mut self, key: String, timings: Timings) {
        self.new_runs.insert(key, timings);
    }

    /// Functions (file, stable name) with a mutant that a test in one of
    /// `test_files` reached, the last time it was tested, with those files.
    pub fn reached_by(
        &self,
        test_files: &[String],
    ) -> BTreeMap<(String, String), BTreeSet<String>> {
        self.functions
            .iter()
            .filter_map(|(key, entry)| {
                let files: BTreeSet<String> = entry
                    .mutants
                    .iter()
                    .flat_map(|m| &m.reached)
                    .filter(|file| test_files.contains(file))
                    .cloned()
                    .collect();
                let mut parts = key.split('\u{0}');
                let id = (parts.next()?.to_string(), parts.next()?.to_string());
                (!files.is_empty()).then_some((id, files))
            })
            .collect()
    }

    /// Mutation score of a function from earlier runs, if its code is unchanged:
    /// (score in percent, number of mutants that counted).
    pub fn score(
        &self,
        relative_file: &str,
        name: &str,
        function_text: &str,
    ) -> Option<(f64, usize)> {
        let hash = super::fnv(&[function_text.as_bytes()]);
        let entry = self.functions.get(&key(relative_file, name, hash))?;
        let detected = entry
            .mutants
            .iter()
            .filter(|m| matches!(m.outcome, Outcome::Killed | Outcome::Timeout))
            .count();
        let counted = entry.mutants.iter().filter(|m| m.outcome.counts()).count();
        (counted > 0).then(|| (detected as f64 * 100.0 / counted as f64, counted))
    }
}

fn cached(
    offset: usize,
    mutant: &dog_food_core::Mutant,
    outcome: Outcome,
    scope: &Scope,
    killers: Option<&Vec<String>>,
) -> CachedMutant {
    let killed_by: Vec<TestFile> = killers
        .filter(|files| outcome == Outcome::Killed && !files.is_empty())
        .into_iter()
        .flatten()
        .filter_map(|path| {
            scope.file_hash(path).map(|hash| TestFile {
                path: path.clone(),
                hash,
            })
        })
        .collect();
    let needs_all_tests = outcome != Outcome::CompileError && killed_by.is_empty();
    CachedMutant {
        offset,
        mutator: mutant.mutator.to_string(),
        replacement: mutant.replacement.clone(),
        outcome,
        build: scope.build,
        killed_by,
        tests: needs_all_tests.then_some(scope.test_set),
        reached: Vec::new(),
        reached_tests: Vec::new(),
        rechecked: false,
    }
}

/// Adds `fresh` to `mutants`, replacing older results for the same mutant.
fn merge(mutants: &mut Vec<CachedMutant>, fresh: Vec<CachedMutant>) {
    for new in fresh {
        mutants.retain(|old| {
            (old.offset, &old.mutator, &old.replacement)
                != (new.offset, &new.mutator, &new.replacement)
        });
        mutants.push(new);
    }
}

fn today() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400)
}

/// Runs `work` while holding the cache folder's lock (shared for reading).
fn locked<T>(
    dir: &Path,
    exclusive: bool,
    work: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let path = dir.join(LOCK);
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    else {
        // No cache folder yet (reading), or a read-only one: nothing to guard.
        return work();
    };
    let taken = if exclusive {
        lock.lock()
    } else {
        lock.lock_shared()
    };
    taken.map_err(|err| format!("{}: {err}", path.display()))?;
    work()
}

fn read(dir: &Path) -> Result<Option<Cache>, String> {
    let Ok(text) = std::fs::read_to_string(dir.join(FILE)) else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&text).ok())
}

/// Writes a temporary file and renames it over the cache, so a reader never
/// sees half a file.
fn write(dir: &Path, cache: &Cache) -> Result<(), String> {
    let path = dir.join(FILE);
    let temporary = dir.join(format!("{FILE}.{}.tmp", std::process::id()));
    std::fs::write(
        &temporary,
        serde_json::to_string(cache).expect("cache serializes"),
    )
    .map_err(|err| format!("{}: {err}", temporary.display()))?;
    std::fs::rename(&temporary, &path).map_err(|err| format!("{}: {err}", path.display()))
}

/// File, name and code hash: two lambdas in one method share a stable name,
/// so the hash tells them apart.
fn key(relative_file: &str, name: &str, function: u64) -> String {
    format!(
        "{}\u{0}{}\u{0}{function:016x}",
        relative_file.replace('\\', "/"),
        stable_name(name)
    )
}

/// One key for a set of functions (their cache keys, in order): `None` when
/// a function's file cannot be read.
pub fn run_key(targets: &[Target], root: &Path) -> Option<String> {
    let mut keys = targets
        .iter()
        .filter(|t| !t.mutants.is_empty())
        .map(|t| identify(t, root).map(|(key, _)| key))
        .collect::<Option<Vec<String>>>()?;
    keys.sort();
    let parts: Vec<&[u8]> = keys.iter().map(|k| k.as_bytes()).collect();
    Some(format!("{:016x}", super::fnv(&parts)))
}

/// The cache key, and the byte where the function's first line starts.
fn identify(target: &Target, root: &Path) -> Option<(String, usize)> {
    let source = std::fs::read_to_string(&target.path).ok()?;
    let (start, text) = function_text(&source, target.start_line, target.end_line)?;
    let relative = absolute(&target.path)
        .strip_prefix(absolute(root))
        .ok()?
        .to_string_lossy()
        .into_owned();
    let hash = super::fnv(&[text.as_bytes()]);
    Some((key(&relative, &target.name, hash), start))
}

/// The byte where line `start` begins, and the text of lines `start..=end`.
pub fn function_text(source: &str, start: usize, end: usize) -> Option<(usize, &str)> {
    let mut offsets = vec![0];
    offsets.extend(source.match_indices('\n').map(|(i, _)| i + 1));
    let from = *offsets.get(start.checked_sub(1)?)?;
    let to = offsets.get(end).copied().unwrap_or(source.len());
    Some((from, &source[from..to]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::sandbox::Kind;

    fn target(root: &Path, file: &str, source: &str) -> Target {
        let path = root.join(file);
        std::fs::write(&path, source).unwrap();
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        Target {
            file: file.into(),
            path,
            name: "C.F".into(),
            start_line: 2,
            end_line: 2,
            complexity: 1,
            reason: "selected",
            mutants,
            suppressions: Vec::new(),
        }
    }

    fn scope(root: &Path, build: u64, test_set: u64) -> Scope {
        Scope::fixed(Kind::CSharp, root, build, test_set)
    }

    #[test]
    fn function_text_cuts_whole_lines() {
        let source = "a\nfn f() {\n  1\n}\nz\n";
        assert_eq!(function_text(source, 2, 4), Some((2, "fn f() {\n  1\n}\n")));
        assert_eq!(function_text(source, 9, 9), None);
    }

    #[test]
    fn a_kill_holds_while_its_test_file_is_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("cache");
        std::fs::write(root.path().join("ShopTests.cs"), "class T { void A() {} }").unwrap();
        let target = target(
            root.path(),
            "Shop.cs",
            "class C {\n  int F(int a) { return a + 1; }\n}\n",
        );
        let outcomes = HashMap::from([(0, Outcome::Killed)]);
        let killers = KilledBy::from([(0, vec!["ShopTests.cs".to_string()])]);
        let mut cache = Cache::load(&dir);
        let reached = HashMap::from([(0, BTreeSet::from(["ShopTests.cs".to_string()]))]);
        cache.store(
            &[&target],
            &scope(root.path(), 1, 1),
            &outcomes,
            &killers,
            &reached,
            &HashMap::from([(0, vec!["T.A (ShopTests.cs)".to_string()])]),
        );
        cache.save().unwrap();
        // The function comes back when a test file that reaches it changes.
        let saved = Cache::load(&dir);
        assert_eq!(
            saved.reached_by(&["ShopTests.cs".to_string()]),
            BTreeMap::from([(
                ("Shop.cs".to_string(), "C.F".to_string()),
                BTreeSet::from(["ShopTests.cs".to_string()])
            )])
        );
        assert!(saved.reached_by(&["OtherTests.cs".to_string()]).is_empty());

        // Another test file changing does not matter; the killing one does.
        let mut cache = Cache::load(&dir);
        assert_eq!(
            cache.lookup(&[&target], &scope(root.path(), 1, 2)),
            outcomes
        );
        assert_eq!(
            cache.reached_tests(&[&target], &scope(root.path(), 1, 2)),
            HashMap::from([(0, vec!["T.A (ShopTests.cs)".to_string()])])
        );
        assert_eq!(
            cache.score("Shop.cs", "C.F", "  int F(int a) { return a + 1; }\n"),
            Some((100.0, 1))
        );
        std::fs::write(root.path().join("ShopTests.cs"), "class T { void B() {} }").unwrap();
        assert!(cache
            .lookup(&[&target], &scope(root.path(), 1, 2))
            .is_empty());
    }

    #[test]
    fn survivors_need_every_test_and_the_build_settings_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("cache");
        let target = target(
            root.path(),
            "Shop.cs",
            "class C {\n  int F(int a) { return a + 1; }\n}\n",
        );
        let outcomes = HashMap::from([(0, Outcome::Survived)]);
        let mut cache = Cache::load(&dir);
        cache.store(
            &[&target],
            &scope(root.path(), 1, 7),
            &outcomes,
            &KilledBy::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(
            cache.lookup(&[&target], &scope(root.path(), 1, 7)),
            outcomes
        );
        assert!(cache
            .lookup(&[&target], &scope(root.path(), 1, 8))
            .is_empty());
        assert!(cache
            .lookup(&[&target], &scope(root.path(), 2, 7))
            .is_empty());
        // A changed function is a new entry.
        let changed = target_with(&target, "class C {\n  int F(int a) { return a + 2; }\n}\n");
        assert!(cache
            .lookup(&[&changed], &scope(root.path(), 1, 7))
            .is_empty());
    }

    #[test]
    fn a_survivor_rechecked_against_the_same_tests_is_not_rechecked_again() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("cache");
        let target = target(
            root.path(),
            "Shop.cs",
            "class C {\n  int F(int a) { return a + 1; }\n}\n",
        );
        let survived = HashMap::from([(0, Outcome::Survived)]);
        let store = |cache: &mut Cache, test_set| {
            cache.store(
                &[&target],
                &scope(root.path(), 1, test_set),
                &survived,
                &KilledBy::new(),
                &HashMap::new(),
                &HashMap::new(),
            )
        };
        let mut cache = Cache::load(&dir);
        store(&mut cache, 7);
        assert!(cache
            .rechecked(&[&target], &scope(root.path(), 1, 7))
            .is_empty());
        cache.rerun = true;
        store(&mut cache, 7);
        assert_eq!(
            cache.rechecked(&[&target], &scope(root.path(), 1, 7)),
            HashSet::from([0])
        );
        // Changed tests: the old result, rechecked or not, no longer holds.
        assert!(cache
            .rechecked(&[&target], &scope(root.path(), 1, 8))
            .is_empty());
    }

    #[test]
    fn timings_of_a_run_are_kept_for_the_same_functions() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("cache");
        let one = target(
            root.path(),
            "One.cs",
            "class C {\n  int F(int a) { return a + 1; }\n}\n",
        );
        let key = run_key(std::slice::from_ref(&one), root.path()).unwrap();
        let timings = Timings {
            build_ms: 1200,
            test_runs: 3,
            at: crate::report::now(),
            ..Timings::default()
        };
        let mut cache = Cache::load(&dir);
        cache.remember_timings(key.clone(), timings);
        cache.save().unwrap();
        assert_eq!(Cache::load(&dir).timings(&key), Some(timings));
        let changed = target_with(&one, "class C {\n  int F(int a) { return a + 2; }\n}\n");
        assert_ne!(
            run_key(std::slice::from_ref(&changed), root.path()),
            Some(key)
        );
    }

    fn target_with(old: &Target, source: &str) -> Target {
        std::fs::write(&old.path, source).unwrap();
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        Target {
            file: old.file.clone(),
            path: old.path.clone(),
            name: old.name.clone(),
            start_line: 2,
            end_line: 2,
            complexity: 1,
            reason: "selected",
            mutants,
            suppressions: Vec::new(),
        }
    }

    #[test]
    fn saving_keeps_what_another_run_saved_meanwhile() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("cache");
        let one = target(
            root.path(),
            "One.cs",
            "class C {\n  int F(int a) { return a + 1; }\n}\n",
        );
        let mut two = target(
            root.path(),
            "Two.cs",
            "class C {\n  int F(int a) { return a * 2; }\n}\n",
        );
        two.mutants[0].id = 1;
        let scope = scope(root.path(), 1, 1);
        let mut first = Cache::load(&dir);
        let mut second = Cache::load(&dir);
        first.store(
            &[&one],
            &scope,
            &HashMap::from([(0, Outcome::Survived)]),
            &KilledBy::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        second.store(
            &[&two],
            &scope,
            &HashMap::from([(1, Outcome::NoCoverage)]),
            &KilledBy::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        first.save().unwrap();
        second.save().unwrap();
        let mut both = Cache::load(&dir);
        assert_eq!(both.lookup(&[&one, &two], &scope).len(), 2);
    }
}
