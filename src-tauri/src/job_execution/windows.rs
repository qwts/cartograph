//! The NTFS directory-persistence contract is required, not inferred from a
//! successful flush on an arbitrary filesystem. See SPEC-07's platform sources.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::ptr::null_mut;
use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationByHandleW;

use crate::jobs::JobTransitionError;

// The Win32 volume-information API permits MAX_PATH + 1 UTF-16 code units.
pub(super) type FileSystemName = [u16; 261];

pub(super) fn file_system_name(file: &impl AsRawHandle) -> io::Result<FileSystemName> {
    let mut name = [0; 261];
    // SAFETY: the borrowed file keeps this handle alive for the synchronous
    // call. `name` is a writable array of exactly the supplied UTF-16 capacity;
    // every unused optional output is null and the absent volume buffer has
    // zero capacity. The API neither transfers ownership nor retains pointers.
    #[allow(unsafe_code)]
    let result = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(name)
}

pub(super) fn require_ntfs(name: &FileSystemName) -> Result<(), JobTransitionError> {
    // Match the complete terminated filesystem name; unknown, malformed or
    // unsupported results never get a successful-but-unsynced fallback.
    let length = name
        .iter()
        .position(|unit| *unit == 0)
        .ok_or(JobTransitionError::LockUnavailable)?;
    if name[..length] != [b'N' as u16, b'T' as u16, b'F' as u16, b'S' as u16] {
        return Err(JobTransitionError::LockUnavailable);
    }
    Ok(())
}
