//! The run's debug log: `dgf-mutate.log` next to the sandbox's runs, one
//! timestamped line per step, written fresh by every run.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

struct Log {
    started: Instant,
    file: std::fs::File,
    path: PathBuf,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

/// Starts the log at `path`, replacing a previous run's.
pub fn open(path: &Path) {
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
