//! Compile checks in memory, between builds: the C# engine (`dgf-dup-cs
//! --check`) holds the projects' compilations and, on each request, reads the
//! mutated files again and lists the errors the mutants brought in, in the
//! form the compiler prints them. A request takes a fraction of a second where
//! a build takes seconds, so the mutants that do not compile are found with
//! one build after the search instead of one build per step.
//!
//! It runs no source generators and no MSBuild, so it can miss an error; it
//! never adds one the build would not have, since an error counts only in a
//! member that had none like it without mutants. What it misses, the builds
//! that follow find.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

use super::exec;

/// How long the engine may take to load the projects, plus
/// [`PER_FILE_LOADING`] for each mutated file: on a busy machine a solution
/// with a thousand mutated files takes minutes.
const LOADING: Duration = Duration::from_secs(180);
const PER_FILE_LOADING: Duration = Duration::from_millis(200);
/// How long one check may take, plus [`PER_FILE`] for each file: the first
/// check of a large solution reads every mutated file's errors.
const CHECKING: Duration = Duration::from_secs(60);
const PER_FILE: Duration = Duration::from_millis(150);

/// The engine, ready for checks.
pub struct Checker {
    child: Child,
    input: Option<ChildStdin>,
    lines: Receiver<String>,
    /// How long one check may take, by how many files it reads.
    limit: Duration,
}

impl Checker {
    /// Starts `command` on `files`: each mutated file in the sandbox, and a
    /// file with its code without mutants. `None` when it does not come up.
    pub fn start(mut command: Command, files: &[(PathBuf, PathBuf)]) -> Result<Checker, String> {
        command
            .arg("--check")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|err| format!("could not start the C# engine: {err}"))?;
        let stdout = child.stdout.take().ok_or("no output")?;
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        let files_count = u32::try_from(files.len()).unwrap_or(u32::MAX);
        let mut checker = Checker {
            input: child.stdin.take(),
            child,
            lines,
            limit: CHECKING + PER_FILE * files_count,
        };
        let request = serde_json::json!({
            "files": files.iter().map(|(path, original)| serde_json::json!({
                "path": path, "original": original,
            })).collect::<Vec<_>>(),
        });
        checker.send(&request.to_string())?;
        match checker.next(LOADING + PER_FILE_LOADING * files_count)? {
            line if line == "ready" => Ok(checker),
            other => Err(format!("the C# engine said {other:?}")),
        }
    }

    /// The errors the mutants in the files bring in now, as the compiler
    /// prints them.
    pub fn errors(&mut self) -> Result<Vec<String>, String> {
        self.send("check")?;
        let mut errors = Vec::new();
        loop {
            let line = self.next(self.limit)?;
            if line == "done" {
                return Ok(errors);
            }
            if let Some(error) = line.strip_prefix("error\t") {
                errors.push(error.to_string());
            }
        }
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        let input = self.input.as_mut().ok_or("its input is closed")?;
        writeln!(input, "{line}")
            .and_then(|()| input.flush())
            .map_err(|err| format!("could not ask the C# engine: {err}"))
    }

    fn next(&self, limit: Duration) -> Result<String, String> {
        match self.lines.recv_timeout(limit) {
            Ok(line) => Ok(line),
            Err(RecvTimeoutError::Timeout) => Err("the C# engine did not answer in time".into()),
            Err(RecvTimeoutError::Disconnected) => Err("the C# engine ended".into()),
        }
    }
}

impl Drop for Checker {
    fn drop(&mut self) {
        self.input = None;
        exec::kill_tree(&mut self.child, None);
    }
}

/// Writes each file's code without mutants next to the runs, for the engine
/// to compare with; the pairs to give it.
pub fn originals(
    folder: &Path,
    files: &[(PathBuf, &str)],
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    std::fs::create_dir_all(folder).map_err(|err| format!("{}: {err}", folder.display()))?;
    files
        .iter()
        .enumerate()
        .map(|(index, (path, source))| {
            let original = folder.join(format!("{index}.cs"));
            std::fs::write(&original, source)
                .map_err(|err| format!("{}: {err}", original.display()))?;
            Ok((path.clone(), original))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the engine, in Node: the protocol without Roslyn.
    const FAKE: &str = r#"
const lines = require('readline').createInterface({ input: process.stdin });
let first = true;
let checks = 0;
lines.on('line', (line) => {
  if (first) { first = false; JSON.parse(line).files.length === 1 ? console.log('ready') : process.exit(2); return; }
  if (line !== 'check') return;
  checks += 1;
  if (checks === 1) console.log('error\tC:/s/F.cs(3,5): error CS0029: Cannot convert');
  console.log('done');
});
"#;

    fn fake() -> Option<(tempfile::TempDir, Command)> {
        let found = Command::new("node").arg("--version").output();
        if !found.is_ok_and(|o| o.status.success()) {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake.cjs");
        std::fs::write(&script, FAKE).unwrap();
        let mut command = Command::new("node");
        command.arg(&script);
        Some((dir, command))
    }

    #[test]
    fn a_check_lists_the_errors_then_none_once_they_are_gone() {
        let Some((dir, command)) = fake() else { return };
        let files = originals(dir.path(), &[(PathBuf::from("F.cs"), "class F {}")]).unwrap();
        assert_eq!(std::fs::read_to_string(&files[0].1).unwrap(), "class F {}");
        let mut checker = Checker::start(command, &files).unwrap();
        assert_eq!(
            checker.errors().unwrap(),
            ["C:/s/F.cs(3,5): error CS0029: Cannot convert"]
        );
        assert!(checker.errors().unwrap().is_empty());
    }

    #[test]
    fn an_engine_that_does_not_come_up_is_an_error_not_a_wait() {
        let Some((_dir, command)) = fake() else {
            return;
        };
        assert!(Checker::start(command, &[]).is_err());
    }
}
