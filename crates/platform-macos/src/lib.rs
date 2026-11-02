//! macOS filesystem probes. Written against plain Unix `stat` first, with APFS
//! detail layered on, so the crate also builds on other Unixes. Linux support is
//! not promised by this; it is just what the shared code allows.

#![cfg(unix)]

#[cfg(target_os = "macos")]
mod bulk;

use std::ffi::CString;
use std::fs::Metadata;
use std::mem;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use gigsift_core::probe::{FileId, FileProbe};
use gigsift_core::{RecycleError, VolumeSpace};

#[cfg(target_os = "macos")]
pub use bulk::{BulkKind, read_directory};

/// Skipped when scanning from `/`: other volumes, plus the view of the data
/// volume that firmlinks already expose at `/Users` and friends.
pub const EXCLUDED_ROOTS: &[&str] = &["/System/Volumes", "/Volumes", "/dev", "/net", "/home"];

pub fn probe_file(path: &Path, meta: &Metadata) -> FileProbe {
    FileProbe {
        allocated_bytes: meta.blocks() * 512,
        id: Some(FileId {
            volume: meta.dev(),
            file: meta.ino(),
        }),
        links: u32::try_from(meta.nlink()).unwrap_or(u32::MAX),
        placeholder: is_placeholder(meta),
        clone: clone_info(path),
    }
}

pub fn volume_id(_path: &Path, meta: &Metadata) -> Option<u64> {
    Some(meta.dev())
}

/// How full the volume holding `path` is, in bytes. `f_bavail` is what a normal user
/// can write. Purgeable space isn't included, so macOS may be able to free more.
pub fn volume_space(path: &Path) -> Option<VolumeSpace> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` is plain data; all-zero is a valid value.
    let mut stat: libc::statvfs = unsafe { mem::zeroed() };
    // SAFETY: the path is NUL-terminated and `stat` is writable.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let unit = stat.f_frsize as u64;
    Some(VolumeSpace {
        volume: std::fs::metadata(path).ok()?.dev(),
        total_bytes: stat.f_blocks as u64 * unit,
        available_bytes: stat.f_bavail as u64 * unit,
    })
}

/// True for iCloud dataless files and directories. Listing a dataless directory
/// makes the system download it, so the scanner must not descend into one.
#[cfg(target_os = "macos")]
pub fn is_placeholder(meta: &Metadata) -> bool {
    use std::os::macos::fs::MetadataExt as _;
    const SF_DATALESS: u32 = 0x4000_0000;
    meta.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
pub fn is_placeholder(_meta: &Metadata) -> bool {
    false
}

/// Moves `path` to the Trash and returns where it went. `NSFileManager` fails on
/// volumes without a Trash rather than deleting, which is the behavior gigsift
/// relies on: there is no fallback here that destroys data.
#[cfg(target_os = "macos")]
pub fn move_to_trash(path: &Path) -> Result<Option<PathBuf>, RecycleError> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    /// `NSFeatureUnsupportedError`, what Cocoa returns for a volume with no Trash.
    const FEATURE_UNSUPPORTED: isize = 3328;

    let text = path
        .to_str()
        .ok_or_else(|| RecycleError::Failed("path is not valid Unicode".to_owned()))?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(text));
    let manager = NSFileManager::defaultManager();
    let mut location = None;
    manager
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut location))
        .map_err(|error| {
            let detail = error.localizedDescription().to_string();
            if error.code() == FEATURE_UNSUPPORTED {
                RecycleError::Unavailable(detail)
            } else {
                RecycleError::Failed(detail)
            }
        })?;
    Ok(location
        .and_then(|url| url.path())
        .map(|path| PathBuf::from(path.to_string())))
}

/// Linux has no native route here yet, so nothing is ever moved or deleted.
#[cfg(not(target_os = "macos"))]
pub fn move_to_trash(_path: &Path) -> Result<Option<PathBuf>, RecycleError> {
    Err(RecycleError::Unavailable(
        "this platform is not supported yet".to_owned(),
    ))
}

#[cfg(target_os = "macos")]
fn clone_info(path: &Path) -> Option<gigsift_core::probe::CloneInfo> {
    apfs::clone_info(path)
}

#[cfg(not(target_os = "macos"))]
fn clone_info(_path: &Path) -> Option<gigsift_core::probe::CloneInfo> {
    None
}

#[cfg(target_os = "macos")]
mod apfs {
    use std::ffi::{CString, c_void};
    use std::mem;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use gigsift_core::probe::CloneInfo;

    // From <sys/attr.h> and <sys/stat.h>; libc does not carry the extended set.
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x0000_0008;
    const ATTR_CMNEXT_CLONEID: u32 = 0x0000_0100;
    const ATTR_CMNEXT_EXT_FLAGS: u32 = 0x0000_0200;
    const ATTR_CMNEXT_CLONE_REFCNT: u32 = 0x0000_1000;
    const FSOPT_NOFOLLOW: u32 = 0x0000_0001;
    const FSOPT_ATTR_CMN_EXTENDED: u32 = 0x0000_0020;
    const EF_MAY_SHARE_BLOCKS: u64 = 0x0000_0001;

    const REQUESTED: u32 = ATTR_CMNEXT_PRIVATESIZE
        | ATTR_CMNEXT_CLONEID
        | ATTR_CMNEXT_EXT_FLAGS
        | ATTR_CMNEXT_CLONE_REFCNT;

    /// Layout of the reply: attributes follow in ascending bit order.
    #[repr(C, packed(4))]
    #[derive(Default)]
    struct Reply {
        length: u32,
        returned_common: u32,
        returned_vol: u32,
        returned_dir: u32,
        returned_file: u32,
        returned_ext: u32,
        private_size: i64,
        clone_id: u64,
        ext_flags: u64,
        clone_refcnt: u32,
    }

    /// Volumes without clone tracking leave the attributes out of the reply, and
    /// those volumes hold no clones, so this returning `None` is the honest answer.
    pub fn clone_info(path: &Path) -> Option<CloneInfo> {
        let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
        // SAFETY: `attrlist` is plain data; all-zero is a valid value.
        let mut request: libc::attrlist = unsafe { mem::zeroed() };
        request.bitmapcount = ATTR_BIT_MAP_COUNT;
        request.commonattr = ATTR_CMN_RETURNED_ATTRS;
        request.forkattr = REQUESTED;

        let mut reply = Reply::default();
        // SAFETY: the pointers are valid for the call and `reply` is writable for
        // the size passed. The kernel writes no more than that.
        let status = unsafe {
            libc::getattrlist(
                c_path.as_ptr(),
                (&raw mut request).cast::<c_void>(),
                (&raw mut reply).cast::<c_void>(),
                mem::size_of::<Reply>(),
                FSOPT_NOFOLLOW | FSOPT_ATTR_CMN_EXTENDED,
            )
        };
        if status != 0 || reply.returned_ext & REQUESTED != REQUESTED {
            return None;
        }
        if reply.ext_flags & EF_MAY_SHARE_BLOCKS == 0 {
            return None;
        }
        Some(CloneInfo {
            id: reply.clone_id,
            private_bytes: u64::try_from(reply.private_size).ok()?,
            references: reply.clone_refcnt,
        })
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Touches the real Trash, so only CI runners opt in.
    #[test]
    fn a_folder_lands_in_the_trash() {
        if std::env::var_os("GIGSIFT_TEST_REAL_TRASH").is_none() {
            return;
        }
        let home = std::env::home_dir().unwrap();
        let dir = home.join(format!("gigsift-trash-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("inner/file.bin"), vec![7u8; 4096]).unwrap();

        let location = move_to_trash(&dir).unwrap();

        assert!(!dir.exists());
        let location = location.expect("macOS should say where it went");
        assert!(location.exists(), "{}", location.display());
    }
}
