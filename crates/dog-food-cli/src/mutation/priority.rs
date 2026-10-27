//! `--low-priority`: dgf, and the builds and test runs it starts, run below
//! normal priority, so other work on a shared machine stays responsive.
//! Child processes inherit the priority; MSBuild's reused build nodes get
//! `-lowPriority` from the build command.

#[cfg(windows)]
pub fn lower() -> Result<(), String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn SetPriorityClass(process: isize, class: u32) -> i32;
    }
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x4000;
    // SAFETY: both calls take no pointers; the handle is this process's
    // pseudo-handle, which needs no closing.
    let changed = unsafe { SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS) };
    if changed == 0 {
        return Err(format!(
            "could not lower the priority: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub fn lower() -> Result<(), String> {
    extern "C" {
        fn nice(increment: i32) -> i32;
    }
    // SAFETY: nice takes a number and changes only this process's priority.
    unsafe {
        nice(10);
    }
    Ok(())
}

#[cfg(not(any(windows, unix)))]
pub fn lower() -> Result<(), String> {
    Ok(())
}
