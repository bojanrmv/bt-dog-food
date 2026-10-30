//! Progress lines go to stderr only, and only when asked for when stderr is
//! not a terminal: a report read from stdout, by a pipe or an agent, is the
//! same with or without them.

use std::path::Path;
use std::process::{Command, Output};

fn analyze(progress: Option<&str>) -> Output {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../dog-food-core/tests/fixtures");
    let mut command = Command::new(env!("CARGO_BIN_EXE_dgf"));
    command
        .args(["analyze", "--no-config", "--format", "json"])
        .arg(&fixtures)
        .env_remove("DGF_PROGRESS");
    if let Some(value) = progress {
        command.env("DGF_PROGRESS", value);
    }
    command.output().expect("dgf runs")
}

#[test]
fn steps_are_shown_on_stderr_when_asked_for() {
    let output = analyze(Some("1"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("  finding source files: "), "{stderr}");
    assert!(stderr.contains("  scoring every function: "), "{stderr}");
}

#[test]
fn the_report_is_the_same_with_or_without_them() {
    let shown = analyze(Some("1"));
    let quiet = analyze(None);
    let report = |o: &Output| {
        let mut json: serde_json::Value = serde_json::from_slice(&o.stdout).expect("JSON report");
        // The run's own times differ between any two runs.
        json["generatedAt"] = serde_json::Value::Null;
        json["summary"]["durationMs"] = serde_json::Value::Null;
        json
    };
    assert_eq!(report(&shown), report(&quiet));
}

#[test]
fn nothing_extra_when_stderr_is_not_a_terminal() {
    let output = analyze(None);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("finding source files"), "{stderr}");
}
