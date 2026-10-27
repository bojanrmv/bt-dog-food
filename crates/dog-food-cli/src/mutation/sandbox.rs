//! The sandbox: a copy of the parts of the repository the tests need, in the
//! temp folder, where the chosen functions get mutant switches. Your working
//! tree is never touched.
//!
//! The copy is kept between runs and brought up to date file by file, so the
//! build output in it stays and the next build only compiles what changed.
//! Links (junctions on Windows) are linked again, never copied.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use dog_food_core::mutate::schemata;
use dog_food_core::Mutant;

use super::Outcome;
use crate::discover::absolute;
use crate::mutate::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    CSharp,
    /// TypeScript and JavaScript, tested with Vitest or Jest.
    Node,
}

impl Kind {
    pub fn of(path: &Path) -> Option<Kind> {
        match path.extension()?.to_str()? {
            "cs" => Some(Kind::CSharp),
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Some(Kind::Node),
            _ => None,
        }
    }

    fn folder(self) -> &'static str {
        match self {
            Kind::CSharp => "cs",
            Kind::Node => "node",
        }
    }
}

/// One file to rewrite: where it lives in the repository and its mutants.
pub struct FileMutants {
    pub relative: PathBuf,
    pub source: String,
    pub mutants: Vec<Mutant>,
    /// Line ranges of the chosen functions in this file.
    pub functions: Vec<(usize, usize)>,
}

/// Folders never copied: build output, dependencies, tool state.
pub const SKIP: &[&str] = &[
    ".git",
    ".dgf",
    ".angular",
    ".vs",
    ".idea",
    "bin",
    "obj",
    "TestResults",
    "node_modules",
    "target",
    "dist",
    "coverage",
];

/// Files dgf writes into the sandbox itself, kept when it is brought up to date.
const OWN: &[&str] = &["dgf-tests.sln", "dgf-mutated.sln", super::dotnet::RESTORED];

/// The mutants still to test, grouped per file.
pub fn group(
    targets: &[&Target],
    root: &Path,
    decided: &HashMap<usize, Outcome>,
) -> Result<Vec<FileMutants>, String> {
    // Per file: its open mutants and the line ranges of its functions.
    type Open = (Vec<Mutant>, Vec<(usize, usize)>);
    let mut by_file: BTreeMap<PathBuf, Open> = BTreeMap::new();
    for target in targets {
        let open = target
            .mutants
            .iter()
            .filter(|m| !decided.contains_key(&m.id))
            .cloned();
        let entry = by_file.entry(absolute(&target.path)).or_default();
        entry.0.extend(open);
        entry.1.push((target.start_line, target.end_line));
    }
    let root = absolute(root);
    by_file
        .into_iter()
        .filter(|(_, (mutants, _))| !mutants.is_empty())
        .map(|(path, (mutants, functions))| {
            let relative = path
                .strip_prefix(&root)
                .map_err(|_| format!("{} is outside {}", path.display(), root.display()))?
                .to_path_buf();
            let source = std::fs::read_to_string(&path)
                .map_err(|err| format!("{}: {err}", path.display()))?;
            Ok(FileMutants {
                relative,
                source,
                mutants,
                functions,
            })
        })
        .collect()
}

/// A sandbox folder this process holds; other dgf processes use another one.
pub struct Sandbox {
    pub dir: PathBuf,
    _lock: std::fs::File,
}

/// Most sandboxes per repository and language, for dgf runs at the same time.
const SLOTS: usize = 8;

/// The sandbox for `kind`, up to date with `roots` (folders in the
/// repository; `[root]` copies all of it), with the mutant switches written in.
pub fn prepare(
    kind: Kind,
    root: &Path,
    roots: &[PathBuf],
    files: &[FileMutants],
) -> Result<Sandbox, String> {
    let sandbox = claim(root, kind)?;
    let mut wanted = wanted(&absolute(root), roots, kind);
    // The mutated files get their text from `write`, which leaves a file (and
    // its time) alone when the text is the same as last run's.
    for file in files {
        wanted.insert(file.relative.clone(), Source::Mutated);
    }
    sync(&wanted, &sandbox.dir)?;
    write(kind, files, &sandbox.dir)?;
    Ok(sandbox)
}

/// Takes the first free sandbox slot, or waits for the first one.
fn claim(root: &Path, kind: Kind) -> Result<Sandbox, String> {
    let base = sandbox_dir(root, kind);
    std::fs::create_dir_all(base.parent().expect("sandbox has a parent"))
        .map_err(|err| err.to_string())?;
    let slot = |n: usize| {
        let name = base
            .file_name()
            .expect("sandbox has a name")
            .to_string_lossy();
        if n == 0 {
            base.clone()
        } else {
            base.with_file_name(format!("{name}-{}", n + 1))
        }
    };
    for n in 0..SLOTS {
        let dir = slot(n);
        let lock = lock_file(&dir)?;
        if lock.try_lock().is_ok() {
            return Ok(Sandbox { dir, _lock: lock });
        }
    }
    eprintln!("  every sandbox is in use by other dgf runs; waiting for one...");
    let dir = slot(0);
    let lock = lock_file(&dir)?;
    lock.lock().map_err(|err| err.to_string())?;
    Ok(Sandbox { dir, _lock: lock })
}

fn lock_file(dir: &Path) -> Result<std::fs::File, String> {
    let path = dir.with_extension("lock");
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|err| format!("{}: {err}", path.display()))
}

/// `%TEMP%/dgf-mutate/<folder>-<hash of its path>-<kind>`: outside the
/// repository, so its Directory.Build.props, global.json and package.json are
/// not found twice, and short, so deep build output stays under Windows'
/// 260-character path limit.
/// Every worktree of a repository shares its sandboxes: a new worktree's
/// first run brings an already built sandbox up to date instead of building
/// everything.
fn sandbox_dir(root: &Path, kind: Kind) -> PathBuf {
    let root = crate::git::repository(root).map_or_else(|| absolute(root), |r| absolute(&r));
    let name: String = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(24)
        .collect();
    let hash = super::fnv(&[root.to_string_lossy().as_bytes()]) as u32;
    std::env::temp_dir()
        .join("dgf-mutate")
        .join(format!("{name}-{hash:08x}-{}", kind.folder()))
}

/// What the sandbox holds at a relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// A copy of this file.
    File(PathBuf),
    /// A link to this folder.
    Link(PathBuf),
    /// A file with mutant switches, written after the sync.
    Mutated,
}

/// Everything the sandbox should hold, by path relative to `root`: the
/// `roots` folders, the files next to every folder above them (where
/// Directory.Build.props, global.json, NuGet.config and package.json live),
/// `.config` folders there, and files the project files import from elsewhere.
fn wanted(root: &Path, roots: &[PathBuf], kind: Kind) -> BTreeMap<PathBuf, Source> {
    let mut found = BTreeMap::new();
    for dir in roots {
        add_tree(root, &absolute(dir), &mut found);
        for ancestor in absolute(dir).ancestors().skip(1) {
            if !ancestor.starts_with(root) {
                break;
            }
            add_folder_files(root, ancestor, kind, &mut found);
        }
    }
    add_imports(root, &mut found);
    found
}

/// Every file under `dir` (skipping build output), with links kept as links
/// and node_modules folders linked rather than copied.
fn add_tree(root: &Path, dir: &Path, found: &mut BTreeMap<PathBuf, Source>) {
    let walker = ignore::WalkBuilder::new(dir)
        .standard_filters(false)
        .follow_links(false)
        .filter_entry(|e| !SKIP.contains(&e.file_name().to_string_lossy().as_ref()))
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let modules = path.join("node_modules");
        if entry.file_type().is_some_and(|t| t.is_dir()) && modules.is_dir() {
            found.insert(relative.join("node_modules"), Source::Link(modules));
        }
        if let Some(source) = source_of(path, entry.file_type()) {
            found.insert(relative.to_path_buf(), source);
        }
    }
}

/// A link to a folder stays a link; a file (or a link to one) becomes a copy.
fn source_of(path: &Path, kind: Option<std::fs::FileType>) -> Option<Source> {
    let linked = kind?.is_symlink() || is_link(path);
    // `is_dir` and `is_file` follow links, to what they point at.
    match (linked, path.is_dir()) {
        (true, true) => Some(Source::Link(path.to_path_buf())),
        (_, false) => path.is_file().then(|| Source::File(path.to_path_buf())),
        _ => None,
    }
}

/// Whether `path` is a symbolic link or a junction (Rust reports both as links).
fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// The files directly in `dir`, its `.config` folder, and (for Node) a
/// node_modules folder there, which package resolution looks for upwards.
fn add_folder_files(root: &Path, dir: &Path, kind: Kind, found: &mut BTreeMap<PathBuf, Source>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let name = entry.file_name();
        if name == ".config" {
            add_tree(root, &path, found);
        } else if name == "node_modules" && kind == Kind::Node {
            found.insert(relative, Source::Link(path));
        } else if path.is_file() {
            found.insert(relative, Source::File(path));
        }
    }
}

/// Files that project, .props and .targets files import or include from
/// outside the copied folders (`..\build\Shared.props`), followed until no
/// new file turns up.
fn add_imports(root: &Path, found: &mut BTreeMap<PathBuf, Source>) {
    let mut queue: Vec<PathBuf> = found
        .values()
        .filter_map(|s| match s {
            Source::File(path) if is_msbuild(path) => Some(path.clone()),
            _ => None,
        })
        .collect();
    while let Some(file) = queue.pop() {
        for path in imports(&file) {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            if found.contains_key(relative) {
                continue;
            }
            if path.is_dir() {
                add_tree(root, &path, found);
                continue;
            }
            found.insert(relative.to_path_buf(), Source::File(path.clone()));
            if is_msbuild(&path) {
                queue.push(path);
            }
        }
    }
}

fn is_msbuild(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| matches!(e.to_str(), Some("csproj" | "props" | "targets")))
}

/// Paths in `Project="..."` and `Include="..."` attributes of an MSBuild file
/// that leave its folder. A wildcard gives the folder before it; values with
/// other properties than the file's own folder are skipped.
fn imports(file: &Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let folder = file.parent().unwrap_or(Path::new("."));
    let mut found = Vec::new();
    for attribute in ["Project=\"", "Include=\""] {
        for value in text
            .split(attribute)
            .skip(1)
            .filter_map(|v| v.split('"').next())
        {
            found.extend(value.split(';').filter_map(|v| outside_path(folder, v)));
        }
    }
    found
}

fn outside_path(folder: &Path, value: &str) -> Option<PathBuf> {
    let value = value
        .trim()
        .replace("$(MSBuildThisFileDirectory)", "")
        .replace("$(MSBuildProjectDirectory)", "")
        .replace('\\', "/");
    if !value.contains("..") || value.contains("$(") || value.contains("@(") {
        return None;
    }
    let fixed: Vec<&str> = value
        .split('/')
        .take_while(|part| !part.contains('*') && !part.contains('?'))
        .collect();
    let path = absolute(&folder.join(fixed.join("/")));
    path.exists().then_some(path)
}

/// Brings `dir` up to date with `wanted`: copies new and changed files,
/// links folders, and removes what is no longer wanted. Build output
/// (bin, obj) stays.
fn sync(wanted: &BTreeMap<PathBuf, Source>, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    remove_stale(wanted, dir)?;
    let folders: HashSet<&Path> = wanted.keys().flat_map(|p| p.ancestors().skip(1)).collect();
    remove_orphans(dir, Path::new(""), &folders)?;
    for (relative, source) in wanted {
        let target = dir.join(relative);
        match source {
            Source::File(from) => copy_if_changed(from, &target)?,
            Source::Link(to) => relink(to, &target)?,
            Source::Mutated => {}
        }
    }
    Ok(())
}

/// Deletes folders (with their build output) that no longer hold anything
/// wanted, such as a test project a run no longer uses. Build output inside
/// a wanted folder stays; links are left to `remove_stale`.
fn remove_orphans(dir: &Path, relative: &Path, folders: &HashSet<&Path>) -> Result<(), String> {
    let Ok(entries) = std::fs::read_dir(dir.join(relative)) else {
        return Ok(());
    };
    for entry in entries.flatten().filter(|e| is_own_folder(&e.path())) {
        let path = entry.path();
        let child = relative.join(entry.file_name());
        if folders.contains(child.as_path()) {
            remove_orphans(dir, &child, folders)?;
        } else {
            std::fs::remove_dir_all(&path)
                .map_err(|err| format!("could not remove {}: {err}", path.display()))?;
        }
    }
    Ok(())
}

/// A folder of the sandbox's own: not a link, and not build output.
fn is_own_folder(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    path.is_dir() && !is_link(path) && !SKIP.contains(&name.as_ref())
}

/// Deletes files and links in `dir` that `wanted` does not have. Links are
/// removed, never followed.
fn remove_stale(wanted: &BTreeMap<PathBuf, Source>, dir: &Path) -> Result<(), String> {
    let walker = ignore::WalkBuilder::new(dir)
        .standard_filters(false)
        .follow_links(false)
        .filter_entry(|e| {
            e.depth() == 0
                || !SKIP.contains(&e.file_name().to_string_lossy().as_ref())
                || is_link(e.path())
        })
        .build();
    let stale: Vec<PathBuf> = walker
        .flatten()
        .filter(|e| {
            e.depth() > 0
                && e.file_type()
                    .is_some_and(|t| !t.is_dir() || is_link(e.path()))
        })
        .map(|e| e.into_path())
        .filter(|p| {
            let relative = p.strip_prefix(dir).unwrap_or(p);
            !wanted.contains_key(relative) && !OWN.iter().any(|own| relative == Path::new(own))
        })
        .collect();
    for path in stale {
        remove_entry(&path)?;
    }
    Ok(())
}

fn remove_entry(path: &Path) -> Result<(), String> {
    let removed = if is_link(path) {
        std::fs::remove_dir(path).or_else(|_| std::fs::remove_file(path))
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(|err| format!("could not remove {}: {err}", path.display()))
}

/// Copies `from` unless `to` already has the same size and time.
fn copy_if_changed(from: &Path, to: &Path) -> Result<(), String> {
    let source = std::fs::metadata(from).map_err(|err| format!("{}: {err}", from.display()))?;
    if is_link(to) {
        remove_entry(to)?;
    }
    if let Ok(target) = std::fs::metadata(to) {
        // A fresh checkout (another worktree) gives unchanged files a new
        // time: the copy keeps its old time, so the build stays incremental.
        let same_time = target.modified().ok() == source.modified().ok();
        if target.len() == source.len() && (same_time || same_content(from, to)) {
            return Ok(());
        }
    }
    std::fs::create_dir_all(to.parent().expect("a file has a folder"))
        .map_err(|err| err.to_string())?;
    std::fs::copy(from, to).map_err(|err| format!("{}: {err}", from.display()))?;
    keep_time(to, source.modified().ok())
}

fn same_content(a: &Path, b: &Path) -> bool {
    matches!((std::fs::read(a), std::fs::read(b)), (Ok(x), Ok(y)) if x == y)
}

/// Gives the copy the original's time, so the next sync sees it unchanged.
fn keep_time(path: &Path, time: Option<SystemTime>) -> Result<(), String> {
    let Some(time) = time else {
        return Ok(());
    };
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|err| format!("{}: {err}", path.display()))?;
    file.set_modified(time)
        .map_err(|err| format!("{}: {err}", path.display()))
}

/// Makes `at` a link to `target`, replacing whatever is there unless it is
/// already a link.
fn relink(target: &Path, at: &Path) -> Result<(), String> {
    if is_link(at) {
        return Ok(());
    }
    if at.is_dir() {
        std::fs::remove_dir_all(at)
            .map_err(|err| format!("could not clear {}: {err}", at.display()))?;
    } else if at.exists() {
        remove_entry(at)?;
    }
    std::fs::create_dir_all(at.parent().expect("a link has a folder"))
        .map_err(|err| err.to_string())?;
    link(target, at)
}

#[cfg(windows)]
fn link(target: &Path, at: &Path) -> Result<(), String> {
    // cmd reads `/repos` in `D:/repos` (as git prints paths) as a switch.
    let native = |p: &Path| p.to_string_lossy().replace('/', "\\");
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(native(at))
        .arg(native(target))
        .output()
        .map_err(|err| err.to_string())?;
    if status.status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not link {}: {}",
            at.display(),
            String::from_utf8_lossy(&status.stderr)
        ))
    }
}

#[cfg(not(windows))]
fn link(target: &Path, at: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, at)
        .map_err(|err| format!("could not link {}: {err}", at.display()))
}

const CSHARP_SWITCH: &str = "global::DogFood.Mutation.DgfMutants.Is";
const CSHARP_CLASS: &str = include_str!("../../assets/DgfMutants.cs");
const CSHARP_CLASS_FILE: &str = "DgfMutants.g.cs";
const NODE_SWITCH: &str = "__dgfIs";
const NODE_HELPER_TS: &str = include_str!("../../assets/dgf-mutants.ts.txt");
const NODE_HELPER_JS: &str = include_str!("../../assets/dgf-mutants.js.txt");

/// Writes every file with its switches; for C#, also the switch class into
/// each project with a mutated file. For TypeScript and JavaScript the switch
/// function is appended at the end of the file (function declarations are
/// hoisted). A TypeScript file also gets `// @ts-nocheck` on a first line of
/// its own: a mutant only has to be valid syntax.
pub fn write(kind: Kind, files: &[FileMutants], sandbox: &Path) -> Result<(), String> {
    let mut projects = HashSet::new();
    for file in files {
        let chosen: Vec<&Mutant> = file.mutants.iter().collect();
        let path = sandbox.join(&file.relative);
        let language = dog_food_core::Language::from_path(&path)
            .ok_or_else(|| format!("{}: not a C# or TypeScript file", path.display()))?;
        let text = match kind {
            Kind::CSharp => schemata(&file.source, language, &chosen, &|id| {
                format!("{CSHARP_SWITCH}({id})")
            }),
            Kind::Node => {
                let pragma = if added_lines(&file.relative) > 0 {
                    "// @ts-nocheck\n"
                } else {
                    ""
                };
                format!(
                    "{pragma}{}{}",
                    schemata(&file.source, language, &chosen, &|id| format!(
                        "{NODE_SWITCH}({id})"
                    )),
                    node_helper(&path)
                )
            }
        };
        write_if_changed(&path, &text)?;
        if kind == Kind::CSharp {
            projects.extend(project_of(&path));
        }
    }
    for project in projects {
        write_if_changed(&project.join(CSHARP_CLASS_FILE), CSHARP_CLASS)?;
    }
    Ok(())
}

/// Leaves a file alone (and its time, which incremental builds go by) when
/// it already has `text`.
fn write_if_changed(path: &Path, text: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().expect("a file has a folder"))
        .map_err(|err| err.to_string())?;
    std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}

/// Lines `write` puts above the source: `// @ts-nocheck` in TypeScript.
pub fn added_lines(relative: &Path) -> usize {
    let typed = relative
        .extension()
        .is_some_and(|e| matches!(e.to_str(), Some("ts" | "tsx" | "mts" | "cts")));
    usize::from(typed)
}

fn node_helper(path: &Path) -> &'static str {
    let typed = path
        .extension()
        .is_some_and(|e| matches!(e.to_str(), Some("ts" | "tsx" | "mts" | "cts")));
    if typed {
        NODE_HELPER_TS
    } else {
        NODE_HELPER_JS
    }
}

/// The folder of the nearest .csproj above `file`.
fn project_of(file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .find(|dir| has_csproj(dir))
        .map(Path::to_path_buf)
}

/// The nearest .csproj file above `file`.
pub fn project_file(file: &Path) -> Option<PathBuf> {
    let dir = project_of(file)?;
    let entries = std::fs::read_dir(&dir).ok()?;
    let found = entries
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|ext| ext == "csproj"));
    found
}

fn has_csproj(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| e.path().extension().is_some_and(|ext| ext == "csproj"))
    })
}

/// Lines (1-based) that a mutant's expression spans in `source`.
pub fn site_lines(source: &str, mutant: &Mutant) -> std::ops::RangeInclusive<usize> {
    let line_at = |byte: usize| source[..byte].matches('\n').count() + 1;
    let start = line_at(mutant.site_start);
    // A statement's mutated copy and its `if` are on its first line; the
    // lines below are the original code, with other mutants' switches.
    let end = if mutant.statement {
        start
    } else {
        line_at(mutant.site_end)
    };
    start..=end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (file, text) in files {
            let path = root.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    fn relative_paths(wanted: &BTreeMap<PathBuf, Source>) -> Vec<String> {
        wanted
            .keys()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn the_closure_takes_project_folders_build_files_above_them_and_imports() {
        let root = tree(&[
            ("global.json", "{}"),
            ("src/Directory.Build.props", "<Project/>"),
            (
                "src/Shop/Shop.csproj",
                r#"<Project><Import Project="$(MSBuildThisFileDirectory)..\..\build\Shared.props" /><ItemGroup><None Include="..\..\assets\*.json" /></ItemGroup></Project>"#,
            ),
            ("src/Shop/A.cs", "class A {}"),
            ("src/Shop/bin/Debug/A.dll", "x"),
            ("src/Other/Other.cs", "class O {}"),
            ("build/Shared.props", "<Project/>"),
            ("assets/a.json", "{}"),
            ("docs/notes.md", "x"),
            (".config/dotnet-tools.json", "{}"),
        ]);
        let root = absolute(root.path());
        let wanted = wanted(&root, &[root.join("src/Shop")], Kind::CSharp);
        assert_eq!(
            relative_paths(&wanted),
            [
                ".config/dotnet-tools.json",
                "assets/a.json",
                "build/Shared.props",
                "global.json",
                "src/Directory.Build.props",
                "src/Shop/A.cs",
                "src/Shop/Shop.csproj",
            ]
        );
    }

    #[test]
    fn syncing_copies_changes_removes_leftovers_and_keeps_build_output() {
        let root = tree(&[("src/A.cs", "a"), ("src/B.cs", "b")]);
        let out = tempfile::tempdir().unwrap();
        let dir = out.path().join("sandbox");
        let all = |root: &Path| wanted(&absolute(root), &[absolute(root)], Kind::CSharp);
        sync(&all(root.path()), &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("src/B.cs")).unwrap(), "b");
        std::fs::create_dir_all(dir.join("src/obj")).unwrap();
        std::fs::write(dir.join("src/obj/cache"), "keep").unwrap();
        std::fs::write(dir.join("src/A.cs"), "mutated").unwrap();
        std::fs::remove_file(root.path().join("src/B.cs")).unwrap();
        sync(&all(root.path()), &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("src/A.cs")).unwrap(), "a");
        assert!(!dir.join("src/B.cs").exists());
        assert!(dir.join("src/obj/cache").exists());
        // A folder with nothing wanted left goes, build output and all.
        std::fs::create_dir_all(dir.join("old/Gone.Tests/obj")).unwrap();
        std::fs::write(dir.join("old/Gone.Tests/obj/x"), "x").unwrap();
        sync(&all(root.path()), &dir).unwrap();
        assert!(!dir.join("old").exists());
        assert!(dir.join("src/obj/cache").exists());
    }

    #[test]
    fn links_are_linked_again_never_copied_and_removing_them_keeps_the_target() {
        let root = tree(&[
            ("web/a.ts", "x"),
            ("shared/lib/i.js", "x"),
            ("web/node_modules/x/i.js", "x"),
        ]);
        let base = absolute(root.path());
        link(&base.join("shared/lib"), &base.join("web/lib")).unwrap();
        let out = tempfile::tempdir().unwrap();
        let dir = out.path().join("sandbox");
        let wanted = wanted(&base, &[base.join("web")], Kind::Node);
        assert!(matches!(wanted[Path::new("web/lib")], Source::Link(_)));
        assert!(matches!(
            wanted[&Path::new("web").join("node_modules")],
            Source::Link(_)
        ));
        sync(&wanted, &dir).unwrap();
        assert!(is_link(&dir.join("web/lib")) && dir.join("web/lib/i.js").exists());
        assert!(dir.join("web/node_modules/x/i.js").exists());
        // Bringing it up to date without them removes the links, not their targets.
        sync(&BTreeMap::new(), &dir).unwrap();
        assert!(!dir.join("web/lib").exists());
        assert!(
            base.join("shared/lib/i.js").exists() && base.join("web/node_modules/x/i.js").exists()
        );
    }

    #[test]
    fn two_runs_get_different_sandboxes() {
        let root = tempfile::tempdir().unwrap();
        let first = claim(root.path(), Kind::CSharp).unwrap();
        let second = claim(root.path(), Kind::CSharp).unwrap();
        assert_ne!(first.dir, second.dir);
        let dir = first.dir.clone();
        drop(first);
        assert_eq!(claim(root.path(), Kind::CSharp).unwrap().dir, dir);
    }

    #[test]
    fn typescript_switches_are_appended_without_moving_lines() {
        let source = "export function f(a: number) {\n  return a > 1;\n}\n";
        let mut mutants: Vec<Mutant> =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::TypeScript)
                .unwrap()
                .into_iter()
                .filter(|m| m.replacement == ">=")
                .collect();
        mutants[0].id = 0;
        let dir = tempfile::tempdir().unwrap();
        let file = FileMutants {
            relative: PathBuf::from("f.ts"),
            source: source.into(),
            mutants,
            functions: vec![(1, 3)],
        };
        write(Kind::Node, &[file], dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join("f.ts")).unwrap();
        assert!(
            text.starts_with(
                "// @ts-nocheck\nexport function f(a: number) {\n  return (__dgfIs(0) ? (a >= 1) : (a > 1));\n}\n"
            ),
            "{text}"
        );
        assert!(text.contains("function __dgfIs(id: number): boolean"));
    }
}
