//! Warm test workers for C#: dog-food's worker process hosts a test project's
//! VSTest adapter on the test project's own runtime and keeps running, so the
//! test assembly loads, compiles and warms up once, and every later test run
//! costs only its tests. A worker serves one run at a time; the pool keeps
//! the idle ones per test assembly for the runs going on side by side.

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::exec::{self, cpu};

/// Longest a worker may take to start and find its tests.
const START: Duration = Duration::from_secs(600);

/// A test project a worker can serve: its test assembly, with the runtime
/// and dependency files the build wrote next to it, and a VSTest adapter;
/// or a Microsoft.Testing.Platform test application (`mtp`), which runs in
/// the platform's server mode.
#[derive(Clone, Debug)]
pub struct Target {
    pub dll: PathBuf,
    pub mtp: bool,
}

impl Target {
    /// `dll`, or why a worker cannot serve it.
    pub fn of(dll: &Path, mtp: bool) -> Result<Target, String> {
        if !dll.is_file() {
            return Err(format!("its build output is not there: {}", dll.display()));
        }
        let target = Target {
            dll: dll.to_path_buf(),
            mtp,
        };
        if mtp {
            return Ok(target);
        }
        for extension in ["runtimeconfig.json", "deps.json"] {
            if !dll.with_extension(extension).is_file() {
                return Err(format!(
                    "its build output has no {extension}, so it is not a test project that runs on modern .NET: {}",
                    dll.display()
                ));
            }
        }
        let folder = dll.parent().unwrap_or(Path::new("."));
        let has_adapter = std::fs::read_dir(folder)
            .map_err(|err| format!("{}: {err}", folder.display()))?
            .flatten()
            .any(|e| {
                let name = e.file_name().to_string_lossy().to_lowercase();
                name.contains("testadapter") && name.ends_with(".dll")
            });
        if !has_adapter {
            return Err(
                "it has no test adapter in its build output (xunit.runner.visualstudio, NUnit3TestAdapter, MSTest.TestAdapter), and is not a Microsoft.Testing.Platform application"
                    .to_string(),
            );
        }
        Ok(target)
    }
}

/// One test's result: all rows of a theory together.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub test: String,
    pub passed: bool,
    pub time: Duration,
    /// The test's source file, when the adapter reports it.
    pub file: String,
}

/// What a worker reported for one request.
#[derive(Debug, Default)]
pub struct Report {
    pub rows: Vec<Row>,
    /// After a plan: the (test, mutant) pairs whose mutant took effect.
    pub activated: BTreeSet<(String, usize)>,
    /// After a plan: failed tests that fail without their mutant too.
    pub polluted: BTreeSet<String>,
    /// Tests that said they started and have reported no result.
    pub running: BTreeSet<String>,
}

#[derive(Debug)]
pub enum Reply {
    Done(Report),
    /// Past the limit: the worker was stopped, with what it reported first.
    /// `busy` when it kept a processor busy all along, as an endless loop does.
    TimedOut {
        busy: bool,
        report: Report,
    },
    /// The worker ended before it finished (a mutant crashed it), with the
    /// tests it reported first.
    Crashed(Report),
}

/// An "activated" line: test, mutant id.
fn activated(line: &str) -> Option<(String, usize)> {
    let (test, id) = line.strip_prefix("activated\t")?.rsplit_once('\t')?;
    Some((test.to_string(), id.trim().parse().ok()?))
}

/// A "result" line: test, passed or failed, milliseconds, source file.
fn row(line: &str) -> Option<Row> {
    let mut parts = line.strip_prefix("result\t")?.split('\t');
    let test = parts.next()?.to_string();
    let passed = parts.next()? == "passed";
    let ms = parts.next()?.trim().parse().unwrap_or(0);
    let file = parts.next().unwrap_or("").trim().to_string();
    Some(Row {
        test,
        passed,
        time: Duration::from_millis(ms),
        file,
    })
}

pub struct Worker {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    job: Option<cpu::Job>,
    /// Whether a test run warmed it up (loaded and compiled the tests' code).
    pub warm: bool,
    /// Where its standard error goes: what the tests write, and why it crashed.
    pub log: PathBuf,
    /// Its name in the debug log: its log file and process id.
    name: String,
    /// Its memory after the last request, for the debug log.
    stats: Option<Stats>,
}

/// A worker's memory after a request: bytes allocated since it started, GC
/// collections per generation, managed heap, working set and its peak.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stats {
    allocated: u64,
    collections: [u64; 3],
    heap: u64,
    working_set: u64,
    peak: u64,
}

fn stats(line: &str) -> Option<Stats> {
    let numbers: Vec<u64> = line
        .strip_prefix("stats\t")?
        .split('\t')
        .map(|n| n.trim().parse().ok())
        .collect::<Option<_>>()?;
    let [allocated, gen0, gen1, gen2, heap, working_set, peak] = numbers[..] else {
        return None;
    };
    Some(Stats {
        allocated,
        collections: [gen0, gen1, gen2],
        heap,
        working_set,
        peak,
    })
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

impl Worker {
    /// Starts dog-food's worker `program` for `target`, with `env` set, and
    /// waits until it found the tests.
    pub fn start(
        program: &Path,
        target: &Target,
        env: &[(&str, String)],
        log: &Path,
    ) -> Result<Worker, String> {
        let errors =
            std::fs::File::create(log).map_err(|err| format!("{}: {err}", log.display()))?;
        let mut command = Command::new("dotnet");
        command.arg("exec");
        // The VSTest worker loads the tests into itself, so it runs on their
        // runtime and dependencies; the platform's worker starts the test
        // application as a process of its own.
        if !target.mtp {
            command
                .arg("--runtimeconfig")
                .arg(target.dll.with_extension("runtimeconfig.json"))
                .arg("--depsfile")
                .arg(target.dll.with_extension("deps.json"));
        }
        command
            .arg(program)
            .arg(&target.dll)
            // Tests run where the test host would run them: in their output folder.
            .current_dir(target.dll.parent().unwrap_or(Path::new(".")))
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(errors);
        let mut child = command
            .spawn()
            .map_err(|err| format!("could not start dog-food's test worker: {err}"))?;
        let job = cpu::Job::of(&child);
        let stdout = child.stdout.take().expect("stdout is piped");
        let stdin = child.stdin.take().expect("stdin is piped");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let started = Instant::now();
        let name = format!(
            "{} pid {}",
            log.file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default(),
            child.id()
        );
        let worker = Worker {
            child,
            stdin,
            lines,
            job,
            warm: false,
            log: log.to_path_buf(),
            name,
            stats: None,
        };
        let ready = worker.lines.recv_timeout(START);
        let took = started.elapsed().as_secs_f64();
        match ready {
            Ok(line) if line.starts_with("ready") => {
                debug!(
                    "worker",
                    "{} started for {}{} in {took:.2} s: {} tests{}",
                    worker.name,
                    target.dll.display(),
                    if target.mtp {
                        " (testing platform)"
                    } else {
                        ""
                    },
                    line.split('\t').nth(1).unwrap_or("?"),
                    if env.is_empty() {
                        String::new()
                    } else {
                        format!(", {env:?}")
                    }
                );
                Ok(worker)
            }
            Ok(line) => {
                debug!(
                    "worker",
                    "{} refused after {took:.2} s: {line}", worker.name
                );
                worker.stop();
                Err(line.strip_prefix("error\t").unwrap_or(&line).to_string())
            }
            Err(_) => {
                debug!("worker", "{} did not start in {took:.2} s", worker.name);
                worker.stop();
                let said = exec::crash_reason(log.parent().unwrap_or(Path::new(".")));
                Err(format!(
                    "dog-food's test worker did not start{}",
                    said.map(|s| format!(": {s}")).unwrap_or_default()
                ))
            }
        }
    }

    /// Sends `request` and waits for its results, at most `limit` (more when
    /// a busy machine held the worker back, as for any test run).
    pub fn request(&mut self, request: &str, limit: Duration) -> Reply {
        let sent = writeln!(self.stdin, "{request}").and_then(|()| self.stdin.flush());
        if sent.is_err() {
            return Reply::Crashed(Report::default());
        }
        let started = Instant::now();
        let before = self.used();
        let warm = self.warm;
        let reply = self.wait(started, before, limit);
        self.trace(request, &reply, started, before, limit, warm);
        reply
    }

    fn wait(&mut self, started: Instant, before: Option<Duration>, limit: Duration) -> Reply {
        let mut report = Report::default();
        loop {
            match self.lines.recv_timeout(Duration::from_millis(20)) {
                Ok(line) if line == "done" => {
                    self.warm = true;
                    return Reply::Done(report);
                }
                Ok(line) => self.read(&line, &mut report),
                Err(RecvTimeoutError::Disconnected) => return Reply::Crashed(report),
                Err(RecvTimeoutError::Timeout) => {}
            }
            if let Some(busy) = self.over(started, before, limit) {
                return Reply::TimedOut { busy, report };
            }
        }
    }

    fn read(&mut self, line: &str, report: &mut Report) {
        if line.starts_with("stats\t") {
            self.stats = stats(line);
        } else if let Some(pair) = activated(line) {
            report.activated.insert(pair);
        } else if let Some(test) = line.strip_prefix("polluted\t") {
            report.polluted.insert(test.trim().to_string());
        } else if let Some(test) = line.strip_prefix("running\t") {
            report.running.insert(test.trim().to_string());
        } else if let Some(row) = row(line) {
            report.running.remove(&row.test);
            report.rows.push(row);
        }
    }

    /// When the request is past its limit: whether it kept a processor busy.
    fn over(&self, started: Instant, before: Option<Duration>, limit: Duration) -> Option<bool> {
        let used = self.used_since(before);
        exec::past_limit(started.elapsed(), used, limit)
            .then(|| used.is_some_and(|used| used >= limit))
    }

    fn used_since(&self, before: Option<Duration>) -> Option<Duration> {
        self.used()
            .zip(before)
            .map(|(now, then)| now.saturating_sub(then))
    }

    fn trace(
        &self,
        request: &str,
        reply: &Reply,
        started: Instant,
        before: Option<Duration>,
        limit: Duration,
        warm: bool,
    ) {
        let (verdict, rows): (&str, &[Row]) = match reply {
            Reply::Done(report) => ("done", &report.rows),
            Reply::TimedOut { busy: true, report } => ("timed out, busy all along", &report.rows),
            Reply::TimedOut {
                busy: false,
                report,
            } => ("timed out", &report.rows),
            Reply::Crashed(report) => ("crashed", &report.rows),
        };
        debug!(
            "request",
            "{} {}: {verdict} in {:.3} s (processor {}, limit {:.1} s, {}): {} tests, {} failed, their own time {:.3} s",
            self.name,
            request.split('\t').next().unwrap_or(request),
            started.elapsed().as_secs_f64(),
            self.used_since(before)
                .map_or("?".into(), |u| format!("{:.3} s", u.as_secs_f64())),
            limit.as_secs_f64(),
            if warm { "warm" } else { "cold" },
            rows.len(),
            rows.iter().filter(|r| !r.passed).count(),
            rows.iter().map(|r| r.time).sum::<Duration>().as_secs_f64()
        );
        if let (Some(s), Reply::Done(_)) = (self.stats, reply) {
            debug!(
                "memory",
                "{}: allocated {:.0} MB so far, gc {}/{}/{}, heap {:.0} MB, working set {:.0} MB (peak {:.0} MB)",
                self.name,
                megabytes(s.allocated),
                s.collections[0],
                s.collections[1],
                s.collections[2],
                megabytes(s.heap),
                megabytes(s.working_set),
                megabytes(s.peak)
            );
        }
    }

    fn used(&self) -> Option<Duration> {
        self.job.as_ref().and_then(cpu::Job::used)
    }

    /// Stops the worker and whatever it started.
    pub fn stop(mut self) {
        exec::kill_tree(&mut self.child, self.job.as_ref());
    }

    /// Asks the worker to end, and stops it when it does not.
    fn quit(mut self) {
        let _ = writeln!(self.stdin, "quit").and_then(|()| self.stdin.flush());
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.stop();
    }
}

/// The idle workers of each test assembly.
pub struct Pool {
    /// The worker that loads a VSTest adapter, found when a test project needs it.
    vstest: OnceLock<PathBuf>,
    /// The worker for Microsoft.Testing.Platform test applications, likewise.
    mtp: OnceLock<PathBuf>,
    logs: PathBuf,
    idle: Mutex<HashMap<PathBuf, Vec<Worker>>>,
    started: AtomicUsize,
}

impl Pool {
    pub fn new(logs: PathBuf) -> Pool {
        Pool {
            vstest: OnceLock::new(),
            mtp: OnceLock::new(),
            logs,
            idle: Mutex::new(HashMap::new()),
            started: AtomicUsize::new(0),
        }
    }

    /// Uses `program` for Microsoft.Testing.Platform test applications.
    pub fn set_mtp(&self, program: PathBuf) {
        let _ = self.mtp.set(program);
    }

    /// Uses `program` for test projects with a VSTest adapter.
    pub fn set_vstest(&self, program: PathBuf) {
        let _ = self.vstest.set(program);
    }

    /// An idle worker for `target`, or a new one.
    pub fn take(&self, target: &Target) -> Result<Worker, String> {
        let idle = self
            .idle
            .lock()
            .ok()
            .and_then(|mut idle| idle.get_mut(&target.dll).and_then(Vec::pop));
        match idle {
            Some(worker) => Ok(worker),
            None => self.start(target, &[]),
        }
    }

    /// Keeps `worker` for the next run of `target`.
    pub fn give(&self, target: &Target, worker: Worker) {
        match self.idle.lock() {
            Ok(mut idle) => idle.entry(target.dll.clone()).or_default().push(worker),
            Err(_) => worker.stop(),
        }
    }

    /// A new worker for `target` with `env`, which the caller stops or gives.
    pub fn start(&self, target: &Target, env: &[(&str, String)]) -> Result<Worker, String> {
        std::fs::create_dir_all(&self.logs).map_err(|err| err.to_string())?;
        let n = self.started.fetch_add(1, Ordering::Relaxed);
        let log = self.logs.join(format!("worker-{n}.log"));
        let program = if target.mtp { &self.mtp } else { &self.vstest };
        let program = program.get().ok_or("dog-food's test worker is not there")?;
        Worker::start(program, target, env, &log)
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let idle = match self.idle.get_mut() {
            Ok(idle) => std::mem::take(idle),
            Err(_) => return,
        };
        for worker in idle.into_values().flatten() {
            worker.quit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_lines_give_rows_and_other_lines_none() {
        assert_eq!(
            row("result\tA.B.c(1)\tfailed\t12\tC:/t/ATests.cs"),
            Some(Row {
                test: "A.B.c(1)".into(),
                passed: false,
                time: Duration::from_millis(12),
                file: "C:/t/ATests.cs".into(),
            })
        );
        assert_eq!(row("result\tA.B.d\tpassed\t3\t").unwrap().file, "");
        assert!(row("done").is_none());
        assert!(row("ready\t4").is_none());
        assert_eq!(
            stats("stats\t1048576\t5\t2\t1\t2097152\t3145728\t4194304"),
            Some(Stats {
                allocated: 1_048_576,
                collections: [5, 2, 1],
                heap: 2_097_152,
                working_set: 3_145_728,
                peak: 4_194_304,
            })
        );
        assert!(stats("stats\t1\t2").is_none());
    }

    #[test]
    fn only_a_built_test_assembly_with_an_adapter_is_served() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("Shop.Tests.dll");
        std::fs::write(&dll, "").unwrap();
        std::fs::write(dir.path().join("Shop.Tests.runtimeconfig.json"), "{}").unwrap();
        std::fs::write(dir.path().join("Shop.Tests.deps.json"), "{}").unwrap();
        assert!(Target::of(&dll, false).is_err(), "no adapter yet");
        // A test application of the testing platform needs none.
        assert!(Target::of(&dll, true).is_ok());
        std::fs::write(
            dir.path().join("xunit.runner.visualstudio.testadapter.dll"),
            "",
        )
        .unwrap();
        assert!(Target::of(&dll, false).is_ok());
        std::fs::remove_file(dir.path().join("Shop.Tests.deps.json")).unwrap();
        assert!(Target::of(&dll, false).is_err(), "no deps.json");
    }
}
