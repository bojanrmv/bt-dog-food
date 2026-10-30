//! The C# semantic engine, `dgf-dup-cs` (in `semantic/`): methods that do
//! the same thing written differently, a loop and its LINQ, `if` and `?:`.
//! It reduces each method to one canonical expression with the compiler and
//! writes dgf-dup's JSON; its classes (type 4, evidence `normalized`) join
//! the syntactic ones in one report.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::{duplicated, CloneClass, Confirmation, Copy, Report};

#[derive(Deserialize)]
struct Found {
    classes: Vec<Class>,
    /// With `--confirm`: classes an input proved wrong, left out.
    #[serde(default)]
    disproved: Vec<Disproved>,
}

#[derive(Deserialize)]
struct Disproved {
    copies: Vec<Place>,
    example: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Class {
    /// `copies` (methods that do the same thing) or `query` (queries on one table that repeat).
    #[serde(default)]
    kind: String,
    #[serde(default)]
    entity: Option<String>,
    copies: Vec<Place>,
    lines: usize,
    template: String,
    saves: i64,
    unite: String,
    #[serde(default)]
    cautions: Vec<String>,
    #[serde(default)]
    rules: Vec<String>,
    #[serde(default)]
    confirmation: Option<Confirmation>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Place {
    file: String,
    start_line: usize,
    end_line: usize,
    function: String,
    /// A query's own code; for methods, read from the file.
    #[serde(default)]
    code: Option<String>,
}

/// How to start the engine: `engine` when given (an executable, or a
/// `.dll` run with `dotnet`), else `DGF_DUP_CS`, else the one shipped beside
/// this binary (in `engine/`, as the packages ship it), else `dgf-dup-cs` on
/// PATH.
pub fn command(engine: Option<&Path>) -> Command {
    let chosen = engine
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("DGF_DUP_CS").map(PathBuf::from))
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            shipped(exe.parent()?)
        })
        .unwrap_or_else(|| PathBuf::from("dgf-dup-cs"));
    if chosen.extension().is_some_and(|e| e == "dll") {
        let mut command = Command::new("dotnet");
        command.arg(chosen);
        command
    } else {
        Command::new(chosen)
    }
}

/// The engine shipped in `engine/` under `dir`, the folder of the binary.
fn shipped(dir: &Path) -> Option<PathBuf> {
    let dll = dir.join("engine").join("dgf-dup-cs.dll");
    dll.is_file().then_some(dll)
}

/// What the engine is asked: scan test code too; run each type-4 class's
/// methods on generated inputs (`--confirm`); and, once the syntactic scan
/// is done, what its C# copies take and give as methods (see [`finish_with`]).
#[derive(Clone, Copy, Default)]
pub struct Ask {
    pub tests: bool,
    pub confirm: bool,
    pub regions: bool,
}

/// What a C# copy takes and gives if it is extracted into a method, from
/// the compiler's data flow: its parameters (locals and parameters it reads
/// that are set before it), what it returns (those it sets that are read
/// after it), the jumps out of it, and whether it needs `this`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signature {
    pub parameters: Vec<String>,
    pub returns: Vec<String>,
    pub exits: Vec<String>,
    pub uses_this: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Region<'a> {
    file: &'a str,
    start_line: usize,
    end_line: usize,
}

/// Runs the engine on `paths` and returns its classes, paths relative to `root`.
pub fn classes(
    engine: Command,
    paths: &[PathBuf],
    root: &Path,
    ask: Ask,
) -> Result<Vec<CloneClass>, String> {
    finish(start(engine, paths, root, ask)?)
}

/// Starts the engine on `paths`, so it runs while the syntactic scan does.
pub fn start(
    mut engine: Command,
    paths: &[PathBuf],
    root: &Path,
    ask: Ask,
) -> Result<Child, String> {
    engine.args(paths).arg("--root").arg(root);
    if ask.tests {
        engine.arg("--tests");
    }
    if ask.confirm {
        engine.arg("--confirm");
    }
    if ask.regions {
        engine.arg("--regions").stdin(Stdio::piped());
    } else {
        engine.stdin(Stdio::null());
    }
    engine
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            format!(
                "could not start the C# engine {:?} ({err}); install it (see docs/duplication/csharp.md) or point --semantic-engine at it",
                engine.get_program()
            )
        })
}

/// Waits for an engine started with [`start`] and reads its classes.
pub fn finish(engine: Child) -> Result<Vec<CloneClass>, String> {
    Ok(read(wait(engine)?)?.0)
}

/// Like [`finish`], for an engine started with `regions`: sends it the
/// first copy of each syntactic C# class in `report`, and sets each class's
/// signature from the answer.
pub fn finish_with(mut engine: Child, report: &mut Report) -> Result<Vec<CloneClass>, String> {
    let asked: Vec<usize> = (0..report.classes.len())
        .filter(|&n| {
            let c = &report.classes[n];
            c.clone_type <= 3 && c.kind == "copies" && c.copies[0].file.ends_with(".cs")
        })
        .collect();
    let regions: Vec<Region> = asked
        .iter()
        .map(|&n| {
            let copy = &report.classes[n].copies[0];
            Region {
                file: &copy.file,
                start_line: copy.start_line,
                end_line: copy.end_line,
            }
        })
        .collect();
    let text = serde_json::to_string(&regions).expect("regions serialize");
    // Written from its own thread: the engine may be writing its report meanwhile.
    let writer = engine.stdin.take().map(|mut stdin| {
        std::thread::spawn(move || {
            use std::io::Write as _;
            let _ = stdin.write_all(text.as_bytes());
        })
    });
    let output = wait(engine)?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let (classes, signatures) = read(output)?;
    for (n, signature) in asked.into_iter().zip(signatures) {
        report.classes[n].signature = signature;
    }
    Ok(classes)
}

fn wait(engine: Child) -> Result<std::process::Output, String> {
    let output = engine
        .wait_with_output()
        .map_err(|err| format!("the C# engine: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "the C# engine failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output)
}

/// The report, and the answer about regions when one follows it.
type Read = (Vec<CloneClass>, Vec<Option<Signature>>);

fn read(output: std::process::Output) -> Result<Read, String> {
    let mut documents =
        serde_json::Deserializer::from_slice(&output.stdout).into_iter::<serde_json::Value>();
    let report = documents
        .next()
        .ok_or("the C# engine wrote no report")?
        .map_err(|err| format!("the C# engine's report: {err}"))?;
    let found: Found =
        serde_json::from_value(report).map_err(|err| format!("the C# engine's report: {err}"))?;
    let signatures = match documents.next() {
        Some(answer) => {
            serde_json::from_value(answer.map_err(|err| format!("the C# engine's regions: {err}"))?)
                .map_err(|err| format!("the C# engine's regions: {err}"))?
        }
        None => Vec::new(),
    };
    for wrong in &found.disproved {
        let functions: Vec<&str> = wrong.copies.iter().map(|c| c.function.as_str()).collect();
        eprintln!(
            "dgf-dup: left out, running them disproved it: {} ({})",
            functions.join(" vs "),
            wrong.example
        );
    }
    Ok((found.classes.into_iter().map(class).collect(), signatures))
}

fn class(c: Class) -> CloneClass {
    let query = c.kind == "query";
    CloneClass {
        id: String::new(),
        kind: if query { "query" } else { "copies" },
        clone_type: if query { 0 } else { 4 },
        evidence: if query { "query" } else { "normalized" },
        entity: c.entity,
        similarity: None,
        copies: c
            .copies
            .into_iter()
            .map(|p| Copy {
                file: p.file,
                start_line: p.start_line,
                end_line: p.end_line,
                function: Some(p.function),
                changed: false,
                code: p.code.unwrap_or_default(),
                parts: Vec::new(),
            })
            .collect(),
        lines: c.lines,
        tokens: 0,
        statements: 0,
        template: c.template,
        holes: Vec::new(),
        saves: c.saves,
        unite: c.unite,
        cautions: c.cautions,
        differences: Vec::new(),
        rules: c.rules,
        confirmation: c.confirmation,
        risk: None,
        signature: None,
    }
}

/// Adds the engine's classes to `report`, but not one whose copies are the
/// same functions a class already has: that one says it with more detail.
pub fn merge(report: &mut Report, classes: Vec<CloneClass>, root: &Path) {
    let functions = |c: &CloneClass| {
        let mut f: Vec<(String, Option<String>)> = c
            .copies
            .iter()
            .map(|copy| (copy.file.clone(), copy.function.clone()))
            .collect();
        f.sort();
        f
    };
    let known: std::collections::BTreeSet<_> = report.classes.iter().map(functions).collect();
    for mut class in classes {
        if class.kind != "query" && known.contains(&functions(&class)) {
            continue;
        }
        for copy in &mut class.copies {
            fill(copy, &mut report.file_lines, root);
        }
        report.classes.push(class);
    }
    report.classes.sort_by_key(|c| std::cmp::Reverse(c.saves));
    report.index(root);
    report.duplicated_lines = duplicated(&report.classes);
}

/// A copy's code (a query brings its own: only its lines, not the
/// method's) and its file's line count.
fn fill(copy: &mut Copy, file_lines: &mut std::collections::BTreeMap<String, usize>, root: &Path) {
    let text = std::fs::read_to_string(root.join(&copy.file)).ok();
    if copy.code.is_empty() {
        if let Some(text) = &text {
            copy.code = crate::find::excerpt(text, copy.start_line, copy.end_line);
        }
    }
    file_lines
        .entry(copy.file.clone())
        .or_insert_with(|| text.map_or(copy.end_line, |t| t.lines().count()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_shipped_beside_the_binary_is_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(shipped(dir.path()), None);
        std::fs::create_dir(dir.path().join("engine")).unwrap();
        let dll = dir.path().join("engine").join("dgf-dup-cs.dll");
        std::fs::write(&dll, "").unwrap();
        assert_eq!(shipped(dir.path()), Some(dll));
    }
}
