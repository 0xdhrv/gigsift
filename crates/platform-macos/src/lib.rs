//! macOS filesystem probes. Written against plain Unix `stat` first, with APFS
//! detail layered on, so the crate also builds on other Unixes. Linux support is
//! not promised by this; it is just what the shared code allows.

#![cfg(unix)]

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use gigsift_core::probe::{FileId, FileProbe};

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
