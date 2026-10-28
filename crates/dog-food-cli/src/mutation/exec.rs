//! Running test commands with a time limit, and reading compiler errors.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::sandbox::{site_lines, FileMutants};
use super::TestRun;
use crate::discover::absolute;

/// Runs `command` with its output discarded; stops it (and the processes it
/// started) when it runs past `limit`.
pub fn run(mut command: Command, limit: Duration) -> Result<TestRun, String> {
    command.stdout(Stdio::null()).stderr(Stdio::null());
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
        let elapsed = started.elapsed();
        let held_back = job
            .as_ref()
            .and_then(cpu::Job::used)
            .is_some_and(|used| used < limit);
        if elapsed > limit && !(held_back && elapsed < limit * 10) {
            kill_tree(&mut child, job.as_ref());
            return Ok(TestRun::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Test runners start processes of their own; stop all of them.
fn kill_tree(child: &mut Child, job: Option<&cpu::Job>) {
    if let Some(job) = job {
        job.terminate();
    }
    if cfg!(windows) {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .output();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Processor time of a process and everything it starts: on Windows a job
/// object, which the processes a child starts join as well. Elsewhere there
/// is none, and only the wall clock counts.
mod cpu {
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
        std::fs::remove_dir_all(&folder)
            .map_err(|err| format!("could not clear {}: {err}", folder.display()))?;
    }
    std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
    Ok(folder)
}

/// Every line of every file in `folder`.
pub fn lines_in(folder: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().is_file())
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
/// every mutant of the innermost chosen function around the line is a suspect.
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
    function_suspects(file, line, located);
}

/// Every mutant of the innermost chosen function around `line`.
fn function_suspects(file: &FileMutants, line: usize, located: &mut Located) {
    let Some(&(start, end)) = file
        .functions
        .iter()
        .filter(|(start, end)| (*start..=*end).contains(&line))
        .min_by_key(|(start, end)| end - start)
    else {
        return;
    };
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
    Some((PathBuf::from(head[..open].trim()), number))
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
        let started = Instant::now();
        let status = run(busy, Duration::from_secs(3)).unwrap();
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
        let started = Instant::now();
        let status = run(sleeper(3), Duration::from_millis(1500)).unwrap();
        if cfg!(windows) {
            // Past the limit, but it hardly used the processor: it finishes.
            assert_eq!(status, TestRun::Passed);
        } else {
            assert_eq!(status, TestRun::TimedOut);
        }
        assert!(started.elapsed() < Duration::from_secs(20));
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
