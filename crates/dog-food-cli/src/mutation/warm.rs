//! A Vitest kept alive between test runs, also the one inside `ng test`. A new
//! `ng test` builds the whole application first, which on a real front end
//! costs tens of seconds per run while the tests take two. The mutants are
//! compiled in behind switches, so nothing needs building again.
//!
//! Every run gets new test processes, so a run sees only its own mutants: they
//! read them from the control file (`DGF_CONTROL`), which is the environment
//! of the current run. The test processes note their ids, so a run that hangs
//! stops without the build being lost.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use super::exec::{self, cpu};
use super::TestRun;

const SERVER: &str = include_str!("../../assets/dgf-vitest-server.mjs");

/// How a line from the server starts.
const MARK: &str = "\u{1}dgf\t";
/// What Angular's builder prints when the build fails: in watch mode it then
/// waits for a change instead of ending.
const BUILD_FAILED: &str = "bundle generation failed";
/// How long a stopped run's test processes get to be noticed as gone.
const STOPPING: Duration = Duration::from_secs(15);
/// No processor time for this long before the runner started: it is waiting
/// for something that will not come (it failed in a way that keeps it alive).
const IDLE_START: Duration = Duration::from_secs(120);

#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// The test runner started.
    Init,
    /// A run ended; whether anything in it failed.
    Done(bool),
    /// It cannot serve, with the reason.
    Refused(String),
    BuildFailed,
}

/// What `line` of the server's output says, if anything.
fn parse(line: &str) -> Option<Line> {
    if let Some(rest) = line.strip_prefix(MARK) {
        let (word, text) = rest.split_once('\t').unwrap_or((rest, ""));
        return match word {
            "init" => Some(Line::Init),
            "done" => Some(Line::Done(text.trim() == "1")),
            "refused" => Some(Line::Refused(text.trim().to_string())),
            _ => None,
        };
    }
    line.contains(BUILD_FAILED).then_some(Line::BuildFailed)
}

/// One run's settings, as the switch reads them, and where it writes its
/// process ids.
fn control_text(env: &[(&str, String)], pids: &Path) -> String {
    let mut all: BTreeMap<&str, String> = env.iter().map(|(k, v)| (*k, v.clone())).collect();
    all.insert("DGF_PIDS", pids.to_string_lossy().into_owned());
    serde_json::to_string(&all).unwrap_or_default()
}

/// The process ids a run's switches noted.
fn noted_pids(text: &str, server: u32) -> Vec<u32> {
    let mut pids: Vec<u32> = text
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .filter(|p| *p != server)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// One run for the server to do.
pub struct WarmRun<'a> {
    /// Test files relative to the runner's root; none means all of them.
    pub files: &'a [String],
    pub env: &'a [(&'a str, String)],
    pub report: &'a Path,
    pub log: &'a Path,
    /// Its output folder.
    pub folder: &'a Path,
}

enum Waited {
    Line(Line),
    OverTime,
    Gone,
}

/// A test runner that stays alive between runs.
pub struct Warm {
    child: Child,
    job: Option<cpu::Job>,
    input: Option<ChildStdin>,
    lines: Receiver<Line>,
    control: PathBuf,
    /// It could not be stopped from the outside and has to go.
    pub stuck: bool,
}

/// How a start ended.
pub enum Started {
    Ready(Warm, TestRun),
    /// Not this way, with the reason: run the tests the cold way.
    Refused(String),
}

/// Writes the reporter dgf hands to the runner; the path it is at.
pub fn server_file(folder: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|err| err.to_string())?;
    let file = folder.join("dgf-vitest-server.mjs");
    std::fs::write(&file, SERVER).map_err(|err| format!("{}: {err}", file.display()))?;
    Ok(file)
}

impl Warm {
    /// Starts `command`, whose reporter is the server, and waits for its
    /// first run, which runs every test.
    pub fn start(
        mut command: Command,
        folder: &Path,
        jobs: usize,
        first: &WarmRun,
        limit: Duration,
    ) -> Started {
        let control = folder.join("control.json");
        let pids = first.folder.join("pids.txt");
        if let Err(err) = std::fs::create_dir_all(folder)
            .and_then(|()| std::fs::write(&control, control_text(first.env, &pids)))
        {
            return Started::Refused(format!("{}: {err}", control.display()));
        }
        command
            .env("DGF_CONTROL", &control)
            .env("DGF_JOBS", jobs.to_string())
            .env("DGF_FIRST_REPORT", first.report)
            .env("DGF_FIRST_LOG", first.log)
            .env("NO_COLOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => return Started::Refused(format!("could not start the runner: {err}")),
        };
        let job = cpu::Job::of(&child);
        let (sender, lines) = channel();
        let log = folder.join("server.log");
        let output = std::fs::File::create(&log).ok();
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let sender = sender.clone();
            let output = output.as_ref().and_then(|f| f.try_clone().ok());
            std::thread::spawn(move || read_lines(stream, output, sender));
        }
        drop(sender);
        let input = child.stdin.take();
        let mut warm = Warm {
            child,
            job,
            input,
            lines,
            control,
            stuck: false,
        };
        let refused =
            |reason: String| Started::Refused(format!("{reason} (its output: {})", log.display()));
        loop {
            match warm.wait(limit, true) {
                Waited::Line(Line::Init) => continue,
                Waited::Line(Line::Done(failed)) => {
                    let status = if failed {
                        TestRun::Failed
                    } else {
                        TestRun::Passed
                    };
                    return Started::Ready(warm, status);
                }
                Waited::Line(Line::BuildFailed) => return refused("the build failed".into()),
                Waited::Line(Line::Refused(reason)) => return refused(reason),
                Waited::OverTime => return refused("no first run in time".into()),
                Waited::Gone => return refused("it ended before its first run".into()),
            }
        }
    }

    /// Does one run. An error means the server is gone and nothing was run.
    pub fn run(&mut self, run: &WarmRun, limit: Duration) -> Result<TestRun, String> {
        let pids = run.folder.join("pids.txt");
        std::fs::write(&self.control, control_text(run.env, &pids))
            .map_err(|err| format!("{}: {err}", self.control.display()))?;
        let request = serde_json::json!({
            "files": run.files,
            "report": run.report,
            "log": run.log,
        });
        let input = self.input.as_mut().ok_or("its input is closed")?;
        writeln!(input, "{request}")
            .and_then(|()| input.flush())
            .map_err(|err| format!("could not send it a run: {err}"))?;
        loop {
            match self.wait(limit, false) {
                Waited::Line(Line::Done(false)) => return Ok(TestRun::Passed),
                Waited::Line(Line::Done(true)) => return Ok(TestRun::Failed),
                Waited::Line(Line::Refused(reason)) => return Err(reason),
                Waited::Line(_) => {}
                Waited::OverTime => {
                    self.stop_run(&pids);
                    return Ok(TestRun::TimedOut);
                }
                Waited::Gone => return Err("it ended during a run".into()),
            }
        }
    }

    /// The next line, or why there will not be one in time. Before the first
    /// run a runner that uses no processor is waiting for nothing.
    fn wait(&mut self, limit: Duration, starting: bool) -> Waited {
        let started = Instant::now();
        let used_before = self.used().unwrap_or_default();
        let mut last = (used_before, Instant::now());
        loop {
            match self.lines.recv_timeout(Duration::from_millis(20)) {
                Ok(line) => return Waited::Line(line),
                Err(RecvTimeoutError::Disconnected) => return Waited::Gone,
                Err(RecvTimeoutError::Timeout) => {}
            }
            let used = self.used();
            let spent = used.map(|u| u.saturating_sub(used_before));
            if exec::past_limit(started.elapsed(), spent, limit) {
                return Waited::OverTime;
            }
            if starting {
                if let Some(now) = used {
                    if now > last.0 + Duration::from_millis(100) {
                        last = (now, Instant::now());
                    } else if last.1.elapsed() > IDLE_START {
                        return Waited::OverTime;
                    }
                }
            }
        }
    }

    fn used(&self) -> Option<Duration> {
        self.job.as_ref().and_then(cpu::Job::used)
    }

    /// Stops the test processes of a run past its limit; the runner notices
    /// and carries on. When that does not end the run, the server is stuck.
    fn stop_run(&mut self, pids: &Path) {
        let text = std::fs::read_to_string(pids).unwrap_or_default();
        let found = noted_pids(&text, self.child.id());
        debug!(
            "warm",
            "a run is over its limit: stopping test processes {found:?}"
        );
        for pid in &found {
            exec::kill_pid(*pid);
        }
        let until = Instant::now() + STOPPING;
        while !found.is_empty() && Instant::now() < until {
            match self.lines.recv_timeout(Duration::from_millis(50)) {
                Ok(Line::Done(_)) => return,
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        debug!("warm", "the stopped run did not end: stopping the runner");
        self.stuck = true;
    }
}

impl Drop for Warm {
    fn drop(&mut self) {
        // Its stdin closing ends it; the rest is for one that is stuck.
        self.input = None;
        if !self.stuck {
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        exec::kill_tree(&mut self.child, self.job.as_ref());
    }
}

/// Copies a stream of the server to `output`, and sends on what it says.
fn read_lines(
    stream: Box<dyn Read + Send>,
    mut output: Option<std::fs::File>,
    sender: Sender<Line>,
) {
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if let Some(file) = output.as_mut() {
            let _ = file.write_all(&bytes);
        }
        let line = String::from_utf8_lossy(&bytes);
        if let Some(said) = parse(line.trim_end()) {
            if sender.send(said).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_lines_are_told_from_the_runners_output() {
        assert_eq!(parse("\u{1}dgf\tinit"), Some(Line::Init));
        assert_eq!(parse("\u{1}dgf\tdone\t1"), Some(Line::Done(true)));
        assert_eq!(parse("\u{1}dgf\tdone\t0"), Some(Line::Done(false)));
        assert_eq!(
            parse("\u{1}dgf\trefused\tVitest 2 cannot"),
            Some(Line::Refused("Vitest 2 cannot".into()))
        );
        assert_eq!(
            parse("Application bundle generation failed. [8.4 seconds]"),
            Some(Line::BuildFailed)
        );
        assert_eq!(
            parse("dgf\tdone\t1"),
            None,
            "a test printing it has no mark"
        );
        assert_eq!(parse("Application bundle generation complete."), None);
    }

    #[test]
    fn only_the_test_processes_are_stopped_never_the_server() {
        assert_eq!(noted_pids("12 7\n12\nx 99", 99), vec![7, 12]);
        assert!(noted_pids("", 99).is_empty());
    }

    #[test]
    fn the_control_file_is_the_runs_environment() {
        let text = control_text(
            &[("DGF_PLAN", "C:\\a b\\plan.txt".into())],
            Path::new("p.txt"),
        );
        let parsed: BTreeMap<String, String> = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["DGF_PLAN"], "C:\\a b\\plan.txt");
        assert_eq!(parsed["DGF_PIDS"], "p.txt");
    }

    /// A stand-in for the runner, in Node: the protocol without Vitest.
    const FAKE: &str = r#"
const fs = require('fs');
const say = (s) => process.stdout.write('\n\u0001dgf\t' + s + '\n');
if (process.env.FAKE === 'build') { console.log('Application bundle generation failed. [1 seconds]'); setInterval(() => {}, 1000); return; }
if (process.env.FAKE === 'exit') { process.exit(3); }
say('init');
say('done\t0');
let hung;
require('readline').createInterface({ input: process.stdin }).on('line', (line) => {
  const { files } = JSON.parse(line);
  const control = JSON.parse(fs.readFileSync(process.env.DGF_CONTROL, 'utf8'));
  if (files[0] === 'die') process.exit(4);
  if (files[0] === 'stuck') return;
  if (files[0] === 'hang') {
    hung = require('child_process').spawn(process.execPath, ['-e', 'while (true) {}']);
    fs.writeFileSync(control.DGF_PIDS, hung.pid + '\n');
    hung.on('exit', () => say('done\t1'));
    return;
  }
  say('done\t' + (control.WANTED === 'fail' ? 1 : 0));
});
"#;

    struct Fake {
        folder: tempfile::TempDir,
        script: PathBuf,
    }

    /// `None` where Node is not installed.
    fn fake() -> Option<Fake> {
        let found = Command::new("node").arg("--version").output();
        if !found.is_ok_and(|o| o.status.success()) {
            return None;
        }
        let folder = tempfile::tempdir().unwrap();
        let script = folder.path().join("fake.cjs");
        std::fs::write(&script, FAKE).unwrap();
        Some(Fake { folder, script })
    }

    impl Fake {
        fn start(&self, kind: &str) -> Started {
            let mut command = Command::new("node");
            command.arg(&self.script).env("FAKE", kind);
            let report = self.folder.path().join("report.json");
            let log = self.folder.path().join("log.txt");
            let first = WarmRun {
                files: &[],
                env: &[],
                report: &report,
                log: &log,
                folder: self.folder.path(),
            };
            Warm::start(
                command,
                self.folder.path(),
                2,
                &first,
                Duration::from_secs(30),
            )
        }

        fn run(
            &self,
            warm: &mut Warm,
            file: &str,
            wanted: &str,
            limit: Duration,
        ) -> Result<TestRun, String> {
            let report = self.folder.path().join("report.json");
            let log = self.folder.path().join("log.txt");
            let files = [file.to_string()];
            warm.run(
                &WarmRun {
                    files: &files,
                    env: &[("WANTED", wanted.to_string())],
                    report: &report,
                    log: &log,
                    folder: self.folder.path(),
                },
                limit,
            )
        }

        fn ready(&self) -> Warm {
            match self.start("ok") {
                Started::Ready(warm, status) => {
                    assert_eq!(status, TestRun::Passed);
                    warm
                }
                Started::Refused(reason) => panic!("{reason}"),
            }
        }
    }

    const LONG: Duration = Duration::from_secs(30);

    #[test]
    fn each_run_reads_its_own_settings_and_says_how_it_went() {
        let Some(fake) = fake() else { return };
        let mut warm = fake.ready();
        assert_eq!(fake.run(&mut warm, "a", "pass", LONG), Ok(TestRun::Passed));
        assert_eq!(fake.run(&mut warm, "a", "fail", LONG), Ok(TestRun::Failed));
        assert_eq!(fake.run(&mut warm, "a", "pass", LONG), Ok(TestRun::Passed));
    }

    #[test]
    fn a_hanging_run_loses_its_test_process_and_the_server_goes_on() {
        let Some(fake) = fake() else { return };
        let mut warm = fake.ready();
        let limit = Duration::from_millis(500);
        assert_eq!(
            fake.run(&mut warm, "hang", "pass", limit),
            Ok(TestRun::TimedOut)
        );
        assert!(!warm.stuck);
        assert_eq!(fake.run(&mut warm, "a", "pass", LONG), Ok(TestRun::Passed));
    }

    #[test]
    fn a_run_nothing_can_stop_marks_the_server_as_stuck() {
        let Some(fake) = fake() else { return };
        let mut warm = fake.ready();
        let limit = Duration::from_millis(100);
        assert_eq!(
            fake.run(&mut warm, "stuck", "pass", limit),
            Ok(TestRun::TimedOut)
        );
        assert!(warm.stuck);
    }

    #[test]
    fn a_server_that_dies_in_a_run_is_an_error_not_a_verdict() {
        let Some(fake) = fake() else { return };
        let mut warm = fake.ready();
        assert!(fake.run(&mut warm, "die", "pass", LONG).is_err());
    }

    #[test]
    fn a_failed_build_or_an_early_end_is_refused_instead_of_waiting() {
        let Some(fake) = fake() else { return };
        for kind in ["build", "exit"] {
            let started = Instant::now();
            assert!(matches!(fake.start(kind), Started::Refused(_)), "{kind}");
            assert!(started.elapsed() < Duration::from_secs(20), "{kind}");
        }
    }
}
