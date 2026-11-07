//! Running test commands with a time limit, and reading compiler errors.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::sandbox::{site_lines, FileMutants};
use super::TestRun;
use crate::discover::absolute;

/// Runs `command` with its output written to `log`; stops it (and the
/// processes it started) when it runs past `limit`.
pub fn run(mut command: Command, limit: Duration, log: &Path) -> Result<TestRun, String> {
    let file = std::fs::File::create(log)
        .map_err(|err| format!("could not create {}: {err}", log.display()))?;
    let copy = file.try_clone().map_err(|err| err.to_string())?;
    command.stdout(file).stderr(copy);
    let child = command
        .spawn()
        .map_err(|err| format!("could not start {:?}: {err}", command.get_program()))?;
    wait(child, limit)
}

/// Runs `command` and returns its standard output, or `None` when it ran
/// past `limit` and was stopped.
pub fn output(mut command: Command, limit: Duration) -> Result<Option<(bool, String)>, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|err| format!("could not start {:?}: {err}", command.get_program()))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = stdout.read_to_end(&mut text);
        String::from_utf8_lossy(&text).into_owned()
    });
    let status = wait(child, limit)?;
    let text = reader.join().unwrap_or_default();
    Ok((status != TestRun::TimedOut).then_some((status == TestRun::Passed, text)))
}

/// Waits for `child`. A run that is past `limit` but has used less than
/// `limit` of processor time was kept waiting by a busy machine, not stuck:
/// it gets more time, up to ten times the limit. One that kept a processor
/// busy all along (an endless loop) is stopped at the limit.
fn wait(mut child: Child, limit: Duration) -> Result<TestRun, String> {
    let started = Instant::now();
    let job = cpu::Job::of(&child);
    loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            return Ok(if status.success() {
                TestRun::Passed
            } else {
                TestRun::Failed
            });
        }
        if past_limit(
            started.elapsed(),
            job.as_ref().and_then(cpu::Job::used),
            limit,
        ) {
            kill_tree(&mut child, job.as_ref());
            return Ok(TestRun::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether a run `elapsed` into its `limit`, having used `used` processor
/// time (when known), is over it: past the limit, unless it used less than
/// the limit of processor time, which a busy machine held back; that gets up
/// to ten times the limit.
pub fn past_limit(elapsed: Duration, used: Option<Duration>, limit: Duration) -> bool {
    let held_back = used.is_some_and(|used| used < limit);
    elapsed > limit && !(held_back && elapsed < limit * 10)
}

/// Test runners start processes of their own; stop all of them.
pub fn kill_tree(child: &mut Child, job: Option<&cpu::Job>) {
    let started = Instant::now();
    if let Some(job) = job {
        job.terminate();
    }
    let terminated = started.elapsed();
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .output();
    }
    let tree = started.elapsed();
    let _ = child.kill();
    let _ = child.wait();
    let total = started.elapsed();
    if total >= Duration::from_secs(1) {
        debug!(
            "stop",
            "pid {}: stopping took {:.2} s (job {:.2} s, process tree {:.2} s, waiting for its end {:.2} s)",
            child.id(),
            total.as_secs_f64(),
            terminated.as_secs_f64(),
            (tree - terminated).as_secs_f64(),
            (total - tree).as_secs_f64()
        );
    }
}

/// Stops one process and what it started.
pub fn kill_pid(pid: u32) {
    let pid = pid.to_string();
    let _ = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/PID", &pid, "/T", "/F"])
            .output()
    } else {
        Command::new("kill").args(["-9", &pid]).output()
    };
}

/// Processor time of a process and everything it starts: on Windows a job
/// object, which the processes a child starts join as well. Elsewhere there
/// is none, and only the wall clock counts.
pub mod cpu {
    use std::process::Child;
    use std::time::Duration;

    #[cfg(windows)]
    pub struct Job(isize);

    #[cfg(windows)]
    mod ffi {
        #[repr(C)]
        #[derive(Default)]
        pub struct Accounting {
            pub total_user: i64,
            pub total_kernel: i64,
            pub period_user: i64,
            pub period_kernel: i64,
            pub page_faults: u32,
            pub processes: u32,
            pub active: u32,
            pub terminated: u32,
        }

        #[link(name = "kernel32")]
        extern "system" {
            pub fn CreateJobObjectW(attributes: *const u8, name: *const u16) -> isize;
            pub fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
            pub fn QueryInformationJobObject(
                job: isize,
                class: i32,
                info: *mut Accounting,
                length: u32,
                returned: *mut u32,
            ) -> i32;
            pub fn TerminateJobObject(job: isize, code: u32) -> i32;
            pub fn CloseHandle(handle: isize) -> i32;
        }

        /// JobObjectBasicAccountingInformation.
        pub const ACCOUNTING: i32 = 1;
    }

    #[cfg(windows)]
    impl Job {
        /// A job holding `child`, or `None` when Windows refuses one.
        pub fn of(child: &Child) -> Option<Job> {
            use std::os::windows::io::AsRawHandle;
            // SAFETY: null attributes and name ask for an unnamed job; the
            // process handle stays valid while `child` lives.
            unsafe {
                let job = ffi::CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job == 0 {
                    return None;
                }
                if ffi::AssignProcessToJobObject(job, child.as_raw_handle() as isize) == 0 {
                    ffi::CloseHandle(job);
                    return None;
                }
                Some(Job(job))
            }
        }

        /// Processor time used so far, user and kernel, by every process
        /// that has been in the job.
        pub fn used(&self) -> Option<Duration> {
            let mut info = ffi::Accounting::default();
            // SAFETY: `info` is the structure this information class fills,
            // with its exact size.
            let ok = unsafe {
                ffi::QueryInformationJobObject(
                    self.0,
                    ffi::ACCOUNTING,
                    &mut info,
                    std::mem::size_of::<ffi::Accounting>() as u32,
                    std::ptr::null_mut(),
                )
            };
            let ticks = info.total_user.saturating_add(info.total_kernel);
            (ok != 0).then(|| Duration::from_nanos(ticks.max(0) as u64 * 100))
        }

        pub fn terminate(&self) {
            // SAFETY: the job handle is open until drop.
            unsafe {
                ffi::TerminateJobObject(self.0, 1);
            }
        }
    }

    #[cfg(windows)]
    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle came from CreateJobObjectW and is closed once.
            unsafe {
                ffi::CloseHandle(self.0);
            }
        }
    }

    #[cfg(not(windows))]
    pub struct Job;

    #[cfg(not(windows))]
    impl Job {
        pub fn of(_child: &Child) -> Option<Job> {
            None
        }

        pub fn used(&self) -> Option<Duration> {
            None
        }

        pub fn terminate(&self) {}
    }
}

/// The folder for this sandbox's run output. A sandbox belongs to one dgf
/// process at a time (it is locked), so its runs never mix with another's.
pub fn runs_folder(sandbox: &Path) -> PathBuf {
    let name = sandbox
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    sandbox.with_file_name(format!("{name}.runs"))
}

/// An empty folder for one run's output files.
pub fn fresh_folder(sandbox: &Path, tag: &str) -> Result<PathBuf, String> {
    let folder = runs_folder(sandbox).join(tag);
    if folder.exists() {
        clear_folder(&folder)
            .map_err(|err| format!("could not clear {}: {err}", folder.display()))?;
    }
    std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
    Ok(folder)
}

/// Removes `folder`. On Windows a folder something else has open (an
/// indexer, a scanner, a test host still closing) is denied for a moment:
/// such a removal is tried again a few times over about a second.
fn clear_folder(folder: &Path) -> std::io::Result<()> {
    let mut wait = Duration::from_millis(50);
    for _ in 0..5 {
        match std::fs::remove_dir_all(folder) {
            Err(err) if held(&err) => std::thread::sleep(wait),
            other => return other,
        }
        wait *= 2;
    }
    std::fs::remove_dir_all(folder)
}

/// Access denied, or a sharing violation (Windows error 32).
fn held(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied || err.raw_os_error() == Some(32)
}

/// The name of a run's output log in its folder (`index` tells apart the
/// commands of one run).
pub fn log_name(index: usize) -> String {
    format!("output-{index}.log")
}

/// Why a run that did not report its tests ended, from the logs in
/// `folder`: the lines that name an error, or else the last lines.
pub fn crash_reason(folder: &Path) -> Option<String> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "log"))
        .collect();
    logs.sort();
    let lines: Vec<String> = logs
        .iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();
    let telling = |l: &&String| {
        let l = l.to_lowercase();
        l.contains("error") || l.contains("no tests found") || l.contains("exception")
    };
    let errors: Vec<&String> = lines.iter().filter(telling).take(3).collect();
    let picked = if errors.is_empty() {
        lines.iter().rev().take(3).rev().collect()
    } else {
        errors
    };
    let text = picked
        .iter()
        .map(|l| l.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    (!text.is_empty()).then(|| clip(&text, 400))
}

/// `text` cut to at most `max` characters.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text.to_string(),
    }
}

/// Every line of every file in `folder`.
pub fn lines_in(folder: &Path) -> Vec<String> {
    lines_of(folder, "")
}

/// The lines of each file in `folder` whose name starts with `prefix`.
pub fn files_of(folder: &Path, prefix: &str) -> Vec<Vec<String>> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_file() && e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        })
        .collect()
}

/// Every line of every file in `folder` whose name starts with `prefix`.
pub fn lines_of(folder: &Path, prefix: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_file() && e.file_name().to_string_lossy().starts_with(prefix))
        .flat_map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Writes a plan: one "test<TAB>mutant" line per test.
pub fn write_plan(path: &Path, plan: &[(String, usize)]) -> Result<(), String> {
    let text: String = plan
        .iter()
        .map(|(test, id)| format!("{test}\t{id}\n"))
        .collect();
    std::fs::write(path, text).map_err(|err| format!("{}: {err}", path.display()))
}

/// What compiler errors say about the mutants.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Located {
    /// Mutants whose expression spans a line with an error: they break the build.
    pub broken: HashSet<usize>,
    /// Mutants of a function with an error on a line without mutants (such as
    /// "use of unassigned variable", which points at the use): one of them is
    /// to blame, but not necessarily all.
    pub suspects: HashSet<usize>,
}

impl Located {
    pub fn is_empty(&self) -> bool {
        self.broken.is_empty() && self.suspects.is_empty()
    }
}

/// Reads `path(line,col): error ...` lines, as dotnet and tsc print them.
/// Relative paths are taken from `base`.
pub fn locate(sandbox: &Path, base: &Path, files: &[FileMutants], errors: &[String]) -> Located {
    let locations: Vec<(PathBuf, usize, bool)> = errors
        .iter()
        .filter_map(|e| {
            let (path, line) = error_location(e)?;
            Some((error_file(base, &path, e), line, is_flow_error(e)))
        })
        .collect();
    let mut located = Located::default();
    for file in files {
        let path = absolute(&sandbox.join(&file.relative));
        let added = super::sandbox::added_lines(&file.relative);
        let named = |p: &PathBuf| *p == path || is_only_file_named(p, &file.relative, files);
        for (_, line, flow) in locations.iter().filter(|(p, _, _)| named(p)) {
            let Some(line) = line.checked_sub(added).filter(|l| *l > 0) else {
                continue;
            };
            if *flow {
                function_suspects(file, line, &mut located);
            } else {
                locate_line(file, line, &mut located);
            }
        }
    }
    located.suspects.retain(|id| !located.broken.contains(id));
    located
}

/// The file an error names. MSBuild gives a path relative to the project
/// (named at the end of the line in `[...]`) when full paths are turned off.
fn error_file(base: &Path, path: &Path, error: &str) -> PathBuf {
    if path.is_absolute() {
        return absolute(path);
    }
    let project = error
        .rfind('[')
        .and_then(|open| error[open + 1..].split(']').next())
        .map(str::trim)
        .filter(|p| p.ends_with("proj"))
        .map(PathBuf::from);
    match project.as_ref().and_then(|p| p.parent()) {
        Some(folder) if folder.is_absolute() => absolute(&folder.join(path)),
        _ => absolute(&base.join(path)),
    }
}

/// An error that names only a file name, found under no project, still
/// belongs to the one mutated file of that name.
fn is_only_file_named(error_path: &Path, relative: &Path, files: &[FileMutants]) -> bool {
    let Some(name) = relative.file_name() else {
        return false;
    };
    let bare = error_path.file_name().is_some_and(|n| n == name) && !error_path.exists();
    bare && files
        .iter()
        .filter(|f| f.relative.file_name() == Some(name))
        .count()
        == 1
}

/// Errors the compiler reports where a variable is used or a function ends,
/// not where the mutant that caused them is: any mutant of the function may
/// be to blame. Definite assignment (CS0165, CS0170, CS0171, CS0177, CS0188,
/// CS0269), a missing return (CS0161) and falling through a case (CS0163);
/// TypeScript's "used before being assigned" (TS2454).
const FLOW_ERRORS: &[&str] = &[
    "CS0161", "CS0163", "CS0165", "CS0170", "CS0171", "CS0177", "CS0188", "CS0269", "TS2454",
];

fn is_flow_error(error: &str) -> bool {
    FLOW_ERRORS
        .iter()
        .any(|code| error.contains(&format!("error {code}")))
}

/// The mutant on `line` breaks the build; several there are suspects (one
/// `Count` -> `Sum` does not make the `==` next to it wrong); with none,
/// every mutant of the whole function around the line is a suspect: a mutant
/// can change a type (`a ?? b` to a nullable `a` in an anonymous object) and
/// the error shows where a lambda further down uses it.
fn locate_line(file: &FileMutants, line: usize, located: &mut Located) {
    let on_line: Vec<usize> = file
        .mutants
        .iter()
        .filter(|m| site_lines(&file.source, m).contains(&line))
        .map(|m| m.id)
        .collect();
    match on_line.as_slice() {
        [] => {}
        [one] => {
            located.broken.insert(*one);
            return;
        }
        several => {
            located.suspects.extend(several);
            return;
        }
    }
    match containing(file, line).last() {
        Some(&(start, end)) => suspects_in(file, start, end, located),
        None => outside_suspects(file, located),
    }
}

/// Every mutant outside the chosen functions.
fn outside_suspects(file: &FileMutants, located: &mut Located) {
    located.suspects.extend(
        file.mutants
            .iter()
            .filter(|m| containing(file, m.line).is_empty())
            .map(|m| m.id),
    );
}

/// Every mutant of the innermost chosen function around `line` that has any.
fn function_suspects(file: &FileMutants, line: usize, located: &mut Located) {
    let before = located.suspects.len();
    for (start, end) in around(file, line) {
        suspects_in(file, start, end, located);
        if located.suspects.len() > before {
            return;
        }
    }
}

/// The chosen functions around `line`, innermost first. A line in none of them
/// is a signature (a missing return is reported on the method's name, above its
/// body): it belongs to the first function below it.
fn around(file: &FileMutants, line: usize) -> Vec<(usize, usize)> {
    let mut functions = containing(file, line);
    if functions.is_empty() {
        functions.extend(
            file.functions
                .iter()
                .copied()
                .filter(|(start, _)| *start > line)
                .min_by_key(|(start, _)| *start),
        );
    }
    functions
}

/// The chosen functions that hold `line`, innermost first.
fn containing(file: &FileMutants, line: usize) -> Vec<(usize, usize)> {
    let mut functions: Vec<(usize, usize)> = file
        .functions
        .iter()
        .copied()
        .filter(|(start, end)| (*start..=*end).contains(&line))
        .collect();
    functions.sort_by_key(|(start, end)| end - start);
    functions
}

fn suspects_in(file: &FileMutants, start: usize, end: usize, located: &mut Located) {
    located.suspects.extend(
        file.mutants
            .iter()
            .filter(|m| (start..=end).contains(&m.line))
            .map(|m| m.id),
    );
}

/// `D:\x\Pricing.cs(22,13): error CS0165: ...` gives the file and line 22.
pub fn error_location(line: &str) -> Option<(PathBuf, usize)> {
    let head = &line[..line.find("): error ")?];
    let open = head.rfind('(')?;
    let number = head[open + 1..].split(',').next()?.trim().parse().ok()?;
    // MSBuild keeps the separators project files were written with: `\` on Linux too.
    let path = head[..open].trim();
    let path = if cfg!(windows) {
        path.to_string()
    } else {
        path.replace('\\', "/")
    };
    Some((PathBuf::from(path), number))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that sleeps: it uses no processor time while it waits.
    fn sleeper(seconds: u32) -> Command {
        if cfg!(windows) {
            let mut command = Command::new("powershell");
            command.args([
                "-NoProfile",
                "-Command",
                &format!("Start-Sleep -Seconds {seconds}"),
            ]);
            command
        } else {
            let mut command = Command::new("sleep");
            command.arg(seconds.to_string());
            command
        }
    }

    #[test]
    fn errors_with_paths_relative_to_the_project_find_their_mutants() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = absolute(dir.path());
        let source = "class P {\n  bool F(bool a, bool b) {\n    return a && b;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let files = vec![FileMutants {
            relative: PathBuf::from("src/Shop/Features/Parser.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 4)],
        }];
        let project = sandbox.join("src/Shop/Shop.csproj");
        let added = super::super::sandbox::added_lines(&files[0].relative);
        let line = 4 + added;
        let relative = format!(
            "Features\\Parser.cs({line},12): error CS0165: Use of unassigned local variable 'n' [{}]",
            project.display()
        );
        let bare =
            format!("Parser.cs({line},12): error CS0165: Use of unassigned local variable 'n'");
        for error in [relative, bare] {
            let located = locate(&sandbox, &sandbox, &files, std::slice::from_ref(&error));
            assert!(!located.is_empty(), "{error}");
        }
        assert_eq!(
            error_file(
                &sandbox,
                Path::new("Features/Parser.cs"),
                &format!("x [{}]", project.display())
            ),
            absolute(&sandbox.join("src/Shop/Features/Parser.cs"))
        );
    }

    #[test]
    fn an_endless_loop_is_stopped_at_the_limit() {
        let busy = if cfg!(windows) {
            let mut command = Command::new("powershell");
            command.args(["-NoProfile", "-Command", "while ($true) { }"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "while :; do :; done"]);
            command
        };
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let status = run(busy, Duration::from_secs(3), &dir.path().join("out.log")).unwrap();
        assert_eq!(status, TestRun::TimedOut);
        // Processor time catches up with the wall clock while it loops; a
        // little slack for a busy machine.
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_run_that_waits_without_working_gets_more_time() {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let status = run(
            sleeper(8),
            Duration::from_secs(4),
            &dir.path().join("out.log"),
        )
        .unwrap();
        if cfg!(windows) {
            // Past the limit, but it hardly used the processor: it finishes.
            assert_eq!(status, TestRun::Passed);
        } else {
            assert_eq!(status, TestRun::TimedOut);
        }
        assert!(started.elapsed() < Duration::from_secs(40));
    }

    #[test]
    fn a_crash_reason_is_the_error_lines_or_else_the_last_lines() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(crash_reason(dir.path()), None);
        let log = dir.path().join(log_name(0));
        std::fs::write(
            &log,
            "building

Error: No tests found matching the following patterns
  x.spec.ts
done
",
        )
        .unwrap();
        assert_eq!(
            crash_reason(dir.path()).as_deref(),
            Some("Error: No tests found matching the following patterns")
        );
        std::fs::write(
            &log,
            "one
two
three
four
",
        )
        .unwrap();
        assert_eq!(
            crash_reason(dir.path()).as_deref(),
            Some("two | three | four")
        );
    }

    #[test]
    fn an_error_in_a_lambda_can_be_caused_by_a_mutant_outside_it() {
        let source = "class C {\n  int F(int? a) {\n    var o = new { T = a ?? 1 };\n    return Run(() =>\n      Use(o.T));\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        assert!(mutants.iter().any(|m| m.line == 3));
        let outside = mutants.iter().filter(|m| m.line == 3).count();
        let file = FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 6), (4, 5)],
        };
        let dir = tempfile::tempdir().unwrap();
        let located = locate(
            dir.path(),
            dir.path(),
            std::slice::from_ref(&file),
            &["C.cs(5,11): error CS1503: Argument 1: cannot convert".to_string()],
        );
        let on_three = file
            .mutants
            .iter()
            .filter(|m| located.suspects.contains(&m.id) && m.line == 3);
        assert_eq!(on_three.count(), outside, "{located:?}");
    }

    #[test]
    fn an_error_on_a_signature_blames_the_function_below_it_and_no_other() {
        let source = "class C {\n  int F(\n    bool a)\n  {\n    if (a) return 1;\n  }\n  bool G(int b)\n  {\n    return b > 1;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let in_f: HashSet<usize> = mutants
            .iter()
            .filter(|m| m.line < 7)
            .map(|m| m.id)
            .collect();
        assert!(!in_f.is_empty() && in_f.len() < mutants.len());
        let file = FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(4, 6), (8, 10)],
        };
        let dir = tempfile::tempdir().unwrap();
        let located = locate(
            dir.path(),
            dir.path(),
            std::slice::from_ref(&file),
            &[
                "C.cs(2,7): error CS0161: 'C.F(bool)': not all code paths return a value"
                    .to_string(),
            ],
        );
        assert_eq!(located.suspects, in_f);
    }

    #[test]
    fn an_error_outside_functions_suspects_the_mutants_outside_them() {
        let source = "class C {\n  public string N { get; set; } = \"x\" + \"y\";\n  public int M = U;\n  bool F(int a) {\n    return a > 1;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let outside: HashSet<usize> = mutants
            .iter()
            .filter(|m| m.line < 4)
            .map(|m| m.id)
            .collect();
        assert!(!outside.is_empty());
        let file = FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(4, 6)],
        };
        let dir = tempfile::tempdir().unwrap();
        let located = locate(
            dir.path(),
            dir.path(),
            std::slice::from_ref(&file),
            &["C.cs(3,18): error CS0103: The name 'U' does not exist".to_string()],
        );
        assert_eq!(located.suspects, outside, "not the function below it");
    }

    #[test]
    fn an_error_in_a_function_with_no_mutants_blames_no_other_function() {
        let source = "class C {\n  bool F(int a) {\n    return a > 1;\n  }\n  int G() {\n    int x;\n    return x;\n  }\n}\n";
        let mutants: Vec<_> =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp)
                .unwrap()
                .into_iter()
                .filter(|m| m.line < 5)
                .collect();
        assert!(!mutants.is_empty());
        let file = FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 4), (5, 8)],
        };
        let dir = tempfile::tempdir().unwrap();
        let located = locate(
            dir.path(),
            dir.path(),
            std::slice::from_ref(&file),
            &["C.cs(7,12): error CS0165: Use of unassigned local variable 'x'".to_string()],
        );
        assert!(
            located.is_empty(),
            "the search takes them all out instead: {located:?}"
        );
    }

    #[test]
    fn errors_on_a_mutant_blame_it_and_others_make_suspects() {
        let source = "class C {\n  int F(bool a, bool b) {\n    int x;\n    var y = !b;\n    if (a && y) x = 1;\n    return x > 0 ? 1 : 0;\n  }\n}\n";
        let mutants =
            dog_food_core::mutate::mutants(source, dog_food_core::Language::CSharp).unwrap();
        let file = FileMutants {
            relative: PathBuf::from("C.cs"),
            source: source.into(),
            mutants,
            functions: vec![(2, 7)],
        };
        let dir = tempfile::tempdir().unwrap();
        let error =
            |line: usize| format!("C.cs({line},5): error CS0019: Operator '>' cannot be applied");
        let unassigned = |line: usize| {
            format!("C.cs({line},5): error CS0165: Use of unassigned local variable 'x'")
        };
        let on_mutant = locate(
            dir.path(),
            dir.path(),
            std::slice::from_ref(&file),
            &[error(4)],
        );
        assert!(on_mutant.broken.len() == 1 && on_mutant.suspects.is_empty());
        // Several mutants on the line: any of them may be the one.
        let shared = locate(
            dir.path(),
            dir.path(),
            std::slice::from_ref(&file),
            &[error(5)],
        );
        let on_five = file.mutants.iter().filter(|m| m.line == 5).count();
        assert!(shared.broken.is_empty() && shared.suspects.len() == on_five && on_five > 1);
        let elsewhere = locate(
            dir.path(),
            dir.path(),
            std::slice::from_ref(&file),
            &[error(3)],
        );
        assert!(elsewhere.broken.is_empty());
        assert_eq!(elsewhere.suspects.len(), file.mutants.len());
        // "Use of unassigned local variable" points at a use: the mutant on
        // that line is not to blame more than any other of the function.
        let flow = locate(
            dir.path(),
            dir.path(),
            std::slice::from_ref(&file),
            &[unassigned(4)],
        );
        assert!(flow.broken.is_empty() && flow.suspects.len() == file.mutants.len());
        assert!(locate(dir.path(), dir.path(), &[file], &["Build failed.".into()]).is_empty());
    }

    #[test]
    fn output_is_read_and_slow_commands_are_stopped() {
        let (shell, flag) = if cfg!(windows) {
            ("cmd", "/C")
        } else {
            ("sh", "-c")
        };
        let mut echo = Command::new(shell);
        echo.args([flag, "echo hello"]);
        let (passed, text) = output(echo, Duration::from_secs(30)).unwrap().unwrap();
        assert!(passed && text.contains("hello"));
        let mut slow = Command::new(shell);
        slow.args([
            flag,
            if cfg!(windows) {
                "ping -n 30 127.0.0.1"
            } else {
                "sleep 30"
            },
        ]);
        assert_eq!(output(slow, Duration::from_millis(300)).unwrap(), None);
    }
}
