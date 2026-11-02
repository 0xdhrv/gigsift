//! Windows filesystem probes, built on Win32 rather than on any Unix assumptions.

#![cfg(windows)]

mod trash;

use std::fs::Metadata;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::ptr;

use gigsift_core::VolumeSpace;
use gigsift_core::probe::{FileId, FileProbe};
pub use trash::move_to_trash;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE, NO_ERROR,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_OFFLINE,
    FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GetCompressedFileSizeW, GetDiskFreeSpaceExW, GetFileInformationByHandle,
    INVALID_FILE_SIZE, OPEN_EXISTING,
};

/// Nothing to exclude: drive letters already separate volumes.
pub const EXCLUDED_ROOTS: &[&str] = &[];

pub fn probe_file(path: &Path, meta: &Metadata) -> FileProbe {
    let wide = to_wide(path);
    let info = file_information(&wide);
    FileProbe {
        allocated_bytes: allocated_bytes(&wide).unwrap_or_else(|| meta.file_size()),
        id: info.map(|i| FileId {
            volume: u64::from(i.dwVolumeSerialNumber),
            file: join(i.nFileIndexHigh, i.nFileIndexLow),
        }),
        links: info.map_or(1, |i| i.nNumberOfLinks),
        placeholder: is_placeholder(meta),
        clone: None,
    }
}

pub fn volume_id(path: &Path, _meta: &Metadata) -> Option<u64> {
    file_information(&to_wide(path)).map(|i| u64::from(i.dwVolumeSerialNumber))
}

/// How full the volume holding `path` is, in bytes, for this user.
pub fn volume_space(path: &Path) -> Option<VolumeSpace> {
    let wide = to_wide(path);
    let (mut available, mut total) = (0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated, and both counters outlive the call.
    let ok =
        unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, ptr::null_mut()) };
    if ok == 0 {
        return None;
    }
    let info = file_information(&wide)?;
    Some(VolumeSpace {
        volume: u64::from(info.dwVolumeSerialNumber),
        total_bytes: total,
        available_bytes: available,
    })
}

/// True for cloud-only OneDrive files and directories. Enumerating a recall-on-
/// access directory downloads it, so the scanner must not descend into one.
pub fn is_placeholder(meta: &Metadata) -> bool {
    meta.file_attributes()
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

/// Accounts for NTFS compression and sparse files. Never opens file data, so it
/// does not hydrate placeholders.
fn allocated_bytes(wide: &[u16]) -> Option<u64> {
    let mut high = 0u32;
    // SAFETY: `wide` is NUL-terminated and `high` outlives the call.
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    // A low word of INVALID_FILE_SIZE is ambiguous until GetLastError confirms it.
    // SAFETY: GetLastError has no preconditions.
    if low == INVALID_FILE_SIZE && unsafe { GetLastError() } != NO_ERROR {
        return None;
    }
    Some(join(high, low))
}

/// Opens with no access rights, which reads metadata without touching contents.
fn file_information(wide: &[u16]) -> Option<BY_HANDLE_FILE_INFORMATION> {
    // SAFETY: `wide` is NUL-terminated; the other arguments are flags or null.
    let handle: HANDLE = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: the struct is plain data; all-zero is a valid value.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { mem::zeroed() };
    // SAFETY: `handle` is open and `info` is writable.
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    // SAFETY: `handle` is open and is not used again.
    unsafe { CloseHandle(handle) };
    (ok != 0).then_some(info)
}

fn join(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Adds the `\\?\` prefix so paths over 260 characters work with raw Win32 calls.
/// That prefix turns off path parsing, so `/` must become `\` here or the call
/// fails on a path that is otherwise fine.
fn to_wide(path: &Path) -> Vec<u16> {
    let mut wide: Vec<u16> = Vec::new();
    let text = path.as_os_str();
    let backslash = b'\\' as u16;
    let already_prefixed = text.encode_wide().take(2).eq([backslash, backslash]);
    if path.is_absolute() && !already_prefixed {
        wide.extend(r"\\?\".encode_utf16());
        wide.extend(text.encode_wide().map(
            |unit| {
                if unit == b'/' as u16 { backslash } else { unit }
            },
        ));
    } else {
        wide.extend(text.encode_wide());
    }
    wide.push(0);
    wide
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide_text(path: &str) -> String {
        let wide = to_wide(Path::new(path));
        String::from_utf16(&wide[..wide.len() - 1]).unwrap()
    }

    #[test]
    fn absolute_paths_get_the_long_path_prefix() {
        assert_eq!(wide_text(r"C:\Users\me"), r"\\?\C:\Users\me");
    }

    #[test]
    fn forward_slashes_are_normalized_under_the_prefix() {
        assert_eq!(
            wide_text(r"C:\Users\me\.cargo/registry/cache"),
            r"\\?\C:\Users\me\.cargo\registry\cache"
        );
    }

    #[test]
    fn prefixed_and_relative_paths_are_left_alone() {
        assert_eq!(wide_text(r"\\?\C:\x"), r"\\?\C:\x");
        assert_eq!(wide_text(r"relative\x"), r"relative\x");
    }

    #[test]
    fn a_folder_reached_through_forward_slashes_can_be_probed() {
        let root = std::env::temp_dir().join("gigsift-probe-test");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let mixed = root.join("a/b");
        let meta = std::fs::metadata(&mixed).unwrap();

        let probe = probe_file(&mixed, &meta);

        std::fs::remove_dir_all(&root).unwrap();
        assert!(probe.id.is_some());
    }
}
