//! Junctions: the folder links Windows makes without extra rights. Made
//! through the file system's own call, so no shell is started for one.

use std::io::{Error, ErrorKind, Result};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

#[link(name = "kernel32")]
extern "system" {
    fn DeviceIoControl(
        device: isize,
        code: u32,
        input: *const u8,
        input_size: u32,
        output: *mut u8,
        output_size: u32,
        returned: *mut u32,
        overlapped: *mut u8,
    ) -> i32;
}

const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// The most a reparse point holds, header included.
const MAXIMUM_SIZE: usize = 16 * 1024;

/// Makes the new folder `at` a junction to the folder `target`.
pub fn create(at: &Path, target: &Path) -> Result<()> {
    let data = reparse_data(&std::path::absolute(target)?)?;
    std::fs::create_dir(at)?;
    let set = set_reparse_point(at, &data);
    if set.is_err() {
        let _ = std::fs::remove_dir(at);
    }
    set
}

fn set_reparse_point(at: &Path, data: &[u8]) -> Result<()> {
    let folder = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(at)?;
    let mut returned = 0u32;
    // SAFETY: `data` is a whole reparse buffer of the size given, the handle
    // is open, and nothing is written back but `returned`.
    let ok = unsafe {
        DeviceIoControl(
            folder.as_raw_handle() as isize,
            FSCTL_SET_REPARSE_POINT,
            data.as_ptr(),
            data.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(Error::last_os_error());
    }
    Ok(())
}

/// The mount point reparse buffer for the absolute folder `target`: a
/// header, then the path as the system reads it (`\??\D:\x`) and as it is
/// shown (`D:\x`), each ended by a zero.
fn reparse_data(target: &Path) -> Result<Vec<u8>> {
    let text = target.to_string_lossy();
    let shown = text.strip_prefix(r"\\?\").unwrap_or(&text);
    if shown.starts_with(r"\\") || shown.starts_with(r"UNC\") {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "a junction cannot point to a network folder",
        ));
    }
    let shown: Vec<u16> = Path::new(shown).as_os_str().encode_wide().collect();
    let read: Vec<u16> = r"\??\"
        .encode_utf16()
        .chain(shown.iter().copied())
        .collect();
    let (read_size, shown_size) = (read.len() * 2, shown.len() * 2);
    let paths_size = read_size + 2 + shown_size + 2;
    if 16 + paths_size > MAXIMUM_SIZE {
        return Err(Error::new(ErrorKind::InvalidInput, "the path is too long"));
    }
    let word = |n: usize| (n as u16).to_le_bytes();
    let mut data = Vec::with_capacity(16 + paths_size);
    data.extend(IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    data.extend(word(8 + paths_size)); // what follows the first 8 bytes
    data.extend(word(0)); // reserved
    data.extend(word(0)); // where the read path starts
    data.extend(word(read_size));
    data.extend(word(read_size + 2)); // where the shown path starts
    data.extend(word(shown_size));
    for unit in read.iter().chain(&[0]).chain(&shown).chain(&[0]) {
        data.extend(unit.to_le_bytes());
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::real;

    #[test]
    fn a_junction_leads_to_its_folder_and_goes_without_it() {
        let dir = tempfile::tempdir().unwrap();
        // A space and a letter outside ASCII, as user folders have.
        let target = dir.path().join("real folder é");
        std::fs::create_dir_all(target.join("sub")).unwrap();
        std::fs::write(target.join("sub/a.txt"), "here").unwrap();
        let at = dir.path().join("link");
        // Forward slashes, as git prints paths.
        let slashed = target.to_string_lossy().replace('\\', "/");
        create(&at, Path::new(&slashed)).unwrap();

        assert_eq!(
            std::fs::read_to_string(at.join("sub/a.txt")).unwrap(),
            "here"
        );
        assert!(std::fs::symlink_metadata(&at)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(real(&at), real(&target));
        assert!(create(&at, &target).is_err(), "over a folder that exists");

        std::fs::remove_dir(&at).unwrap();
        assert!(target.join("sub/a.txt").exists());
    }

    #[test]
    fn a_folder_that_cannot_be_linked_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("link");
        let network = create(&at, Path::new(r"\\server\share\folder"));
        assert_eq!(network.unwrap_err().kind(), ErrorKind::Unsupported);
        assert!(!at.exists());
    }

    #[test]
    fn the_buffer_holds_both_forms_of_the_path() {
        let data = reparse_data(Path::new(r"D:\x")).unwrap();
        let word = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]) as usize;
        assert_eq!(data[..4], IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        assert_eq!(word(4), data.len() - 8);
        // `\??\D:\x` is 8 units, `D:\x` is 4.
        assert_eq!((word(8), word(10), word(12), word(14)), (0, 16, 18, 8));
        let units: Vec<u16> = (16..data.len())
            .step_by(2)
            .map(|at| word(at) as u16)
            .collect();
        let text = String::from_utf16(&units).unwrap();
        assert_eq!(text, "\\??\\D:\\x\0D:\\x\0");
        // A path already in the long form is not prefixed twice.
        assert_eq!(reparse_data(Path::new(r"\\?\D:\x")).unwrap(), data);
    }
}
