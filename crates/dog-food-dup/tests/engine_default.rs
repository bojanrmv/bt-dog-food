//! When `dgf-dup` runs the C# engine without being asked, and what happens
//! when the engine cannot run.

use std::path::Path;
use std::process::{Command, Output};

/// A folder with one source file.
fn folder(file: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(file), "class A { void M() { } }\n").unwrap();
    dir
}

/// Runs `dgf-dup` in `dir`, with an engine that cannot start.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dgf-dup"))
        .args(args)
        .current_dir(dir)
        .env("DGF_DUP_CS", dir.join("no-such-engine"))
        .env("DGF_PROGRESS", "0")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_missing_engine_is_a_note_when_nobody_asked_for_it() {
    let dir = folder("A.cs");
    for format in ["table", "agent", "json"] {
        let output = run(dir.path(), &["--format", format]);
        assert_eq!(output.status.code(), Some(0), "{format}");
        assert!(
            stdout(&output).contains("the C# engine did not run"),
            "{format}: {}",
            stdout(&output)
        );
    }
}

#[test]
fn an_engine_that_fails_is_a_note_too() {
    let dir = folder("A.cs");
    let output = Command::new(env!("CARGO_BIN_EXE_dgf-dup"))
        .current_dir(dir.path())
        // Not there: `dotnet` fails on it, or is not there itself.
        .env("DGF_DUP_CS", dir.path().join("no-such-engine.dll"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("the C# engine did not run"));
}

#[test]
fn a_missing_engine_is_an_error_when_asked_for() {
    let dir = folder("A.cs");
    for flag in ["--semantic", "--confirm"] {
        let output = run(dir.path(), &[flag]);
        assert_eq!(output.status.code(), Some(2), "{flag}");
    }
}

#[test]
fn no_semantic_and_code_without_csharp_do_not_mention_the_engine() {
    let csharp = folder("A.cs");
    let typescript = folder("a.ts");
    for (dir, args) in [
        (csharp.path(), &["--no-semantic"][..]),
        (typescript.path(), &[]),
    ] {
        let output = run(dir, args);
        assert_eq!(output.status.code(), Some(0));
        assert!(!stdout(&output).contains("C# engine"), "{args:?}");
    }
}
