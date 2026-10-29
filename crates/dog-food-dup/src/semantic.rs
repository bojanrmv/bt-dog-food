//! The C# semantic engine, `dgf-dup-cs` (in `semantic/`): methods that do
//! the same thing written differently, a loop and its LINQ, `if` and `?:`.
//! It reduces each method to one canonical expression with the compiler and
//! writes dgf-dup's JSON; its classes (type 4, evidence `normalized`) join
//! the syntactic ones in one report.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::{CloneClass, Copy, Report};

#[derive(Deserialize)]
struct Found {
    classes: Vec<Class>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Class {
    copies: Vec<Place>,
    lines: usize,
    template: String,
    saves: i64,
    unite: String,
    #[serde(default)]
    cautions: Vec<String>,
    #[serde(default)]
    rules: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Place {
    file: String,
    start_line: usize,
    end_line: usize,
    function: String,
}

/// How to start the engine: `engine` when given (an executable, or a
/// `.dll` run with `dotnet`), else `DGF_DUP_CS`, else `dgf-dup-cs` on PATH.
pub fn command(engine: Option<&Path>) -> Command {
    let chosen = engine
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("DGF_DUP_CS").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("dgf-dup-cs"));
    if chosen.extension().is_some_and(|e| e == "dll") {
        let mut command = Command::new("dotnet");
        command.arg(chosen);
        command
    } else {
        Command::new(chosen)
    }
}

/// Runs the engine on `paths` and returns its classes, paths relative to `root`.
pub fn classes(
    mut engine: Command,
    paths: &[PathBuf],
    root: &Path,
    tests: bool,
) -> Result<Vec<CloneClass>, String> {
    engine.args(paths).arg("--root").arg(root);
    if tests {
        engine.arg("--tests");
    }
    let output = engine.output().map_err(|err| {
        format!(
            "could not start the C# engine {:?} ({err}); install it (see docs/duplication.md) or point --semantic-engine at it",
            engine.get_program()
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "the C# engine failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let found: Found = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("the C# engine's report: {err}"))?;
    Ok(found.classes.into_iter().map(class).collect())
}

fn class(c: Class) -> CloneClass {
    CloneClass {
        kind: "copies",
        clone_type: 4,
        evidence: "normalized",
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
    for class in classes {
        if known.contains(&functions(&class)) {
            continue;
        }
        for copy in &class.copies {
            if !report.file_lines.contains_key(&copy.file) {
                let lines = std::fs::read_to_string(root.join(&copy.file))
                    .map(|t| t.lines().count())
                    .unwrap_or(copy.end_line);
                report.file_lines.insert(copy.file.clone(), lines);
            }
        }
        report.classes.push(class);
    }
    report.classes.sort_by_key(|c| std::cmp::Reverse(c.saves));
    report.duplicated_lines = report
        .classes
        .iter()
        .map(|c| c.lines * (c.copies.len() - 1))
        .sum();
}
