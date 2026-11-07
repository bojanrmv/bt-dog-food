//! The run's debug log: `dgf-mutate.log` next to the sandbox's runs, one
//! timestamped line per step, written fresh by every run; the run before it
//! stays as `dgf-mutate.previous.log`. It also samples the machine every few
//! seconds, so a slow step can be told from a machine busy with other work.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};
use std::time::{Duration, Instant};

/// How often the log says how busy the machine is.
const SAMPLE: Duration = Duration::from_secs(10);

struct Log {
    started: Instant,
    file: std::fs::File,
    path: PathBuf,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// Starts the log at `path`, replacing a previous run's.
pub fn open(path: &Path) {
    // A new sandbox's runs folder does not exist yet.
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let _ = std::fs::rename(path, path.with_file_name("dgf-mutate.previous.log"));
    let Ok(file) = std::fs::File::create(path) else {
        return;
    };
    if let Ok(mut log) = LOG.lock() {
        *log = Some(Log {
            started: Instant::now(),
            file,
            path: path.to_path_buf(),
        });
    }
    watch_machine();
}

/// Writes how busy the machine is to the log from now on, for the whole process.
fn watch_machine() {
    static WATCHING: Once = Once::new();
    WATCHING.call_once(|| {
        let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
        write("machine", format_args!("{cores} logical processors"));
        std::thread::spawn(|| {
            let mut before = processor_ticks();
            loop {
                std::thread::sleep(SAMPLE);
                let now = processor_ticks();
                if let Some(text) = machine_text(busy_percent(before, now), memory()) {
                    write("machine", format_args!("{text}"));
                }
                before = now;
            }
        });
    });
}

fn machine_text(busy: Option<f64>, memory: Option<(u64, u64)>) -> Option<String> {
    let busy = busy.map(|b| format!("processors {b:.0}% busy"));
    let memory = memory.map(|(free, total)| {
        format!(
            "memory {} of {} MB free",
            free / (1024 * 1024),
            total / (1024 * 1024)
        )
    });
    match (busy, memory) {
        (Some(busy), Some(memory)) => Some(format!("{busy}, {memory}")),
        (one, other) => one.or(other),
    }
}

/// Of the (busy, total) processor time between two samples, the busy part.
fn busy_percent(before: Option<(u64, u64)>, now: Option<(u64, u64)>) -> Option<f64> {
    let ((busy_then, total_then), (busy_now, total_now)) = (before?, now?);
    let total = total_now.checked_sub(total_then).filter(|t| *t > 0)?;
    Some(busy_now.saturating_sub(busy_then) as f64 * 100.0 / total as f64)
}

/// The machine's (busy, total) processor time so far.
#[cfg(windows)]
fn processor_ticks() -> Option<(u64, u64)> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemTimes(idle: *mut u64, kernel: *mut u64, user: *mut u64) -> i32;
    }
    let (mut idle, mut kernel, mut user) = (0u64, 0u64, 0u64);
    // SAFETY: each pointer is a live 8-byte value, the size of a FILETIME.
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    // Kernel time includes idle time.
    let total = kernel + user;
    (ok != 0).then_some((total.saturating_sub(idle), total))
}

/// The machine's free memory in bytes, when it is known.
pub fn free_memory() -> Option<u64> {
    memory().map(|(free, _)| free)
}

/// The machine's (free, total) memory in bytes.
#[cfg(windows)]
fn memory() -> Option<(u64, u64)> {
    #[repr(C)]
    #[derive(Default)]
    struct Status {
        length: u32,
        load: u32,
        total_physical: u64,
        available_physical: u64,
        total_page_file: u64,
        available_page_file: u64,
        total_virtual: u64,
        available_virtual: u64,
        available_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalMemoryStatusEx(status: *mut Status) -> i32;
    }
    let mut status = Status {
        length: std::mem::size_of::<Status>() as u32,
        ..Status::default()
    };
    // SAFETY: `status` is live and has the length it says.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    (ok != 0).then_some((status.available_physical, status.total_physical))
}

#[cfg(target_os = "linux")]
fn processor_ticks() -> Option<(u64, u64)> {
    stat_ticks(&std::fs::read_to_string("/proc/stat").ok()?)
}

#[cfg(target_os = "linux")]
fn memory() -> Option<(u64, u64)> {
    meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn processor_ticks() -> Option<(u64, u64)> {
    None
}

#[cfg(not(any(windows, target_os = "linux")))]
fn memory() -> Option<(u64, u64)> {
    None
}

/// /proc/stat's first line: user, nice, system, idle, iowait, and more.
#[cfg(any(target_os = "linux", test))]
fn stat_ticks(text: &str) -> Option<(u64, u64)> {
    let ticks: Vec<u64> = text
        .lines()
        .next()?
        .strip_prefix("cpu ")?
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    let total: u64 = ticks.iter().sum();
    let idle = ticks.get(3)? + ticks.get(4).unwrap_or(&0);
    Some((total.saturating_sub(idle), total))
}

/// /proc/meminfo's available and total memory, in bytes.
#[cfg(any(target_os = "linux", test))]
fn meminfo(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| -> Option<u64> {
        let line = text.lines().find(|l| l.starts_with(name))?;
        let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kilobytes * 1024)
    };
    Some((field("MemAvailable:")?, field("MemTotal:")?))
}

pub fn path() -> Option<PathBuf> {
    LOG.lock().ok()?.as_ref().map(|l| l.path.clone())
}

/// dgf's own peak working set, in bytes (Windows only).
#[cfg(windows)]
pub fn peak_memory() -> Option<u64> {
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        size: u32,
        page_faults: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged: usize,
        quota_paged: usize,
        quota_peak_non_paged: usize,
        quota_non_paged: usize,
        page_file: usize,
        peak_page_file: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, size: u32) -> i32;
    }
    let mut counters = Counters {
        size: std::mem::size_of::<Counters>() as u32,
        ..Counters::default()
    };
    // SAFETY: the pseudo handle needs no closing; `counters` has the size it says.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.size) };
    (ok != 0).then_some(counters.peak_working_set as u64)
}

#[cfg(not(windows))]
pub fn peak_memory() -> Option<u64> {
    None
}

pub fn write(area: &str, message: std::fmt::Arguments) {
    if let Ok(mut log) = LOG.lock() {
        if let Some(log) = log.as_mut() {
            let at = log.started.elapsed().as_secs_f64();
            let _ = writeln!(log.file, "{at:9.3}s [{area}] {message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_busy_part_is_of_the_time_between_two_samples() {
        assert_eq!(busy_percent(Some((10, 100)), Some((40, 200))), Some(30.0));
        assert_eq!(busy_percent(Some((10, 100)), Some((10, 100))), None);
        assert_eq!(busy_percent(None, Some((10, 100))), None);
    }

    #[test]
    fn proc_stat_counts_idle_and_waiting_as_not_busy() {
        let text = "cpu  10 0 10 70 10 0 0 0 0 0\ncpu0 10 0 10 70 10 0 0 0 0 0\n";
        assert_eq!(stat_ticks(text), Some((20, 100)));
        assert_eq!(stat_ticks("nothing"), None);
    }

    #[test]
    fn proc_meminfo_gives_available_and_total_bytes() {
        let text = "MemTotal: 2048 kB\nMemFree: 10 kB\nMemAvailable: 1024 kB\n";
        assert_eq!(meminfo(text), Some((1024 * 1024, 2048 * 1024)));
        assert_eq!(meminfo("MemTotal: 2048 kB\n"), None);
    }

    #[test]
    fn machine_line_says_what_is_known() {
        let mb = 1024 * 1024;
        assert_eq!(
            machine_text(Some(72.4), Some((512 * mb, 1024 * mb))).as_deref(),
            Some("processors 72% busy, memory 512 of 1024 MB free")
        );
        assert_eq!(
            machine_text(Some(5.0), None).as_deref(),
            Some("processors 5% busy")
        );
        assert_eq!(machine_text(None, None), None);
    }
}
