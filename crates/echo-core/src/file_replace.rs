//! Replacement of an existing file by a synchronized sibling temporary file.

use std::io;
use std::path::Path;

/// Move `replacement` over `destination` without deleting the last good destination first.
///
/// Both paths must be on the same volume, and `replacement` must already be synchronized.
/// Callers retain ownership of cleanup if replacement fails. `std::fs::rename` also replaces
/// an existing file on Windows, but `ReplaceFileW` is used when the destination exists so the
/// replaced file keeps its attributes and security descriptor; the first write is a plain
/// rename.
pub fn replace_file(replacement: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        replace_file_windows(replacement, destination)
    }

    #[cfg(not(windows))]
    {
        std::fs::rename(replacement, destination)
    }
}

#[cfg(windows)]
fn replace_file_windows(replacement: &Path, destination: &Path) -> io::Result<()> {
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW};
    use windows::core::PCWSTR;

    if !destination.exists() {
        match std::fs::rename(replacement, destination) {
            Ok(()) => return Ok(()),
            Err(error) if !destination.exists() => return Err(error),
            Err(_) => {}
        }
    }

    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let replacement_wide: Vec<u16> = replacement
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();

    // SAFETY: both pointers come from NUL-terminated UTF-16 buffers that outlive the
    // call, and the optional backup name, exclude/reserved arguments are null.
    let replace_result = unsafe {
        ReplaceFileW(
            PCWSTR(destination_wide.as_ptr()),
            PCWSTR(replacement_wide.as_ptr()),
            None,
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    };

    match replace_result {
        Ok(()) => Ok(()),
        Err(_) if !destination.exists() => std::fs::rename(replacement, destination),
        Err(error) => Err(io::Error::other(error)),
    }
}
