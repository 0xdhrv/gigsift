//! Reads a directory with `getattrlistbulk`, which returns every entry's metadata in a
//! few calls, instead of one `lstat` and one `getattrlist` per file.
//!
//! Each entry starts with a mask of the attributes it holds, then packs exactly those, in
//! bit order, except that `ATTR_CMN_ERROR` always comes first. Layout varies by entry, so
//! the parser follows the mask rather than assuming positions, and reads through
//! bounds-checked slices so a reply it misunderstands is an error, not a bad number.

use std::cell::RefCell;
use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io;
use std::mem;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use gigsift_core::probe::{CloneInfo, FileId, FileProbe};

const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0002;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
const ATTR_CMNEXT_CLONEID: u32 = 0x0000_0100;
const ATTR_CMNEXT_EXT_FLAGS: u32 = 0x0000_0200;
const ATTR_CMNEXT_CLONE_REFCNT: u32 = 0x0000_1000;
const FSOPT_ATTR_CMN_EXTENDED: u64 = 0x0000_0020;
const EF_MAY_SHARE_BLOCKS: u64 = 0x0000_0001;
const EF_SHARES_ALL_BLOCKS: u64 = 0x0000_0040;
const SF_DATALESS: u32 = 0x4000_0000;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

/// `ATTR_CMNEXT_PRIVATESIZE` is left out on purpose. It makes APFS walk each file's
/// extents, which takes most of a scan's time, and the flags answer it for most files.
const CLONE_ATTRS: u32 = ATTR_CMNEXT_CLONEID | ATTR_CMNEXT_EXT_FLAGS | ATTR_CMNEXT_CLONE_REFCNT;

/// Room for a few hundred entries a call. Eight-byte words give the alignment it needs.
const BUFFER_WORDS: usize = 32 * 1024;

thread_local! {
    static BUFFER: RefCell<Vec<u64>> = RefCell::new(vec![0; BUFFER_WORDS]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkKind {
    /// A regular file, already fully measured.
    File {
        logical_bytes: u64,
        probe: FileProbe,
    },
    /// Whether this crosses into another volume isn't known here: replies can describe
    /// the folder a volume is mounted over, so that takes a real `lstat`.
    Directory {
        placeholder: bool,
    },
    Symlink,
    /// A socket, device, or anything else, which needs a normal `lstat`.
    Other,
}

/// Reads one packed reply, failing rather than running off the end.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        let end = self.at.checked_add(length).ok_or_else(malformed)?;
        let bytes = self.bytes.get(self.at..end).ok_or_else(malformed)?;
        self.at = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> io::Result<u32> {
        let bytes = self.take(4)?.try_into().map_err(|_| malformed())?;
        Ok(u32::from_ne_bytes(bytes))
    }

    fn u64(&mut self) -> io::Result<u64> {
        let bytes = self.take(8)?.try_into().map_err(|_| malformed())?;
        Ok(u64::from_ne_bytes(bytes))
    }
}

fn malformed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "unreadable getattrlistbulk reply",
    )
}

impl Reader<'_> {
    /// The attribute, if the mask says this entry holds it.
    fn u32_if(&mut self, present: bool) -> io::Result<Option<u32>> {
        present.then(|| self.u32()).transpose()
    }

    fn u64_if(&mut self, present: bool) -> io::Result<Option<u64>> {
        present.then(|| self.u64()).transpose()
    }
}

/// Parses one entry. `visit` gets the name, then what it is or the OS error for it.
fn parse_entry(
    directory_path: &Path,
    entry: &[u8],
    visit: &mut impl FnMut(&OsStr, io::Result<BulkKind>),
) -> io::Result<()> {
    let mut reader = Reader {
        bytes: entry,
        at: 4,
    };
    let common = reader.u32()?;
    let _volume = reader.u32()?;
    let _directory = reader.u32()?;
    let file = reader.u32()?;
    let extended = reader.u32()?;
    let has = |mask: u32, bit: u32| mask & bit != 0;

    let error = reader.u32_if(has(common, ATTR_CMN_ERROR))?;
    if !has(common, ATTR_CMN_NAME) {
        return Err(malformed());
    }
    let name_at = reader.at;
    let name_offset = reader.u32()? as usize;
    let name_length = reader.u32()? as usize;
    let name_start = name_at.checked_add(name_offset).ok_or_else(malformed)?;
    let name_end = name_start.checked_add(name_length).ok_or_else(malformed)?;
    let raw_name = entry.get(name_start..name_end).ok_or_else(malformed)?;
    let name = OsStr::from_bytes(raw_name.strip_suffix(&[0]).unwrap_or(raw_name));

    if let Some(code) = error.filter(|code| *code != 0) {
        visit(name, Err(io::Error::from_raw_os_error(code as i32)));
        return Ok(());
    }

    let device = reader.u32_if(has(common, ATTR_CMN_DEVID))?.map(u64::from);
    let object_type = reader.u32_if(has(common, ATTR_CMN_OBJTYPE))?;
    let flags = reader.u32_if(has(common, ATTR_CMN_FLAGS))?.unwrap_or(0);
    let file_id = reader.u64_if(has(common, ATTR_CMN_FILEID))?;
    let placeholder = flags & SF_DATALESS != 0;

    let kind = match (object_type, device) {
        (Some(VDIR), _) => BulkKind::Directory { placeholder },
        (Some(VREG), Some(device)) => {
            let links = reader.u32_if(has(file, ATTR_FILE_LINKCOUNT))?;
            let logical = reader.u64_if(has(file, ATTR_FILE_TOTALSIZE))?;
            let allocated = reader.u64_if(has(file, ATTR_FILE_ALLOCSIZE))?;
            let clone_id = reader.u64_if(has(extended, ATTR_CMNEXT_CLONEID))?;
            let extended_flags = reader.u64_if(has(extended, ATTR_CMNEXT_EXT_FLAGS))?;
            let references = reader.u32_if(has(extended, ATTR_CMNEXT_CLONE_REFCNT))?;
            match (links, logical, allocated) {
                (Some(links), Some(logical_bytes), Some(allocated_bytes)) => BulkKind::File {
                    logical_bytes,
                    probe: FileProbe {
                        allocated_bytes,
                        id: file_id.map(|file| FileId {
                            volume: device,
                            file,
                        }),
                        links,
                        placeholder,
                        clone: clone_info(
                            directory_path,
                            name,
                            clone_id,
                            extended_flags,
                            references,
                        ),
                    },
                },
                // Nothing to measure with, so this entry takes the slow path.
                _ => BulkKind::Other,
            }
        }
        (Some(VLNK), _) => BulkKind::Symlink,
        _ => BulkKind::Other,
    };
    visit(name, Ok(kind));
    Ok(())
}

/// Works out what a file owns from its flags. Across 1.8M real files these held without
/// exception: unshared files own everything, and files sharing all blocks own nothing.
/// Only partly shared files need the expensive lookup. Volumes without clone tracking
/// leave these attributes out, and hold no clones to account for.
fn clone_info(
    directory: &Path,
    name: &OsStr,
    id: Option<u64>,
    flags: Option<u64>,
    references: Option<u32>,
) -> Option<CloneInfo> {
    let (id, flags, references) = (id?, flags?, references?);
    if flags & EF_MAY_SHARE_BLOCKS == 0 {
        return None;
    }
    if flags & EF_SHARES_ALL_BLOCKS == 0 {
        return crate::apfs::clone_info(&directory.join(name));
    }
    Some(CloneInfo {
        id,
        private_bytes: 0,
        references,
    })
}

/// Lists `path`, calling `visit` for each entry except `.` and `..`, which the call
/// never returns. An `Unsupported` error means use a plain directory read instead.
pub fn read_directory(
    path: &Path,
    visit: &mut impl FnMut(&OsStr, io::Result<BulkKind>),
) -> io::Result<()> {
    let directory = File::open(path)?;
    // SAFETY: `attrlist` is plain data; all-zero is a valid value.
    let mut request: libc::attrlist = unsafe { mem::zeroed() };
    request.bitmapcount = ATTR_BIT_MAP_COUNT;
    request.commonattr = ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_NAME
        | ATTR_CMN_ERROR
        | ATTR_CMN_OBJTYPE
        | ATTR_CMN_DEVID
        | ATTR_CMN_FILEID
        | ATTR_CMN_FLAGS;
    request.fileattr = ATTR_FILE_LINKCOUNT | ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE;
    request.forkattr = CLONE_ATTRS;

    BUFFER.with_borrow_mut(|buffer| {
        loop {
            let capacity = buffer.len() * mem::size_of::<u64>();
            // SAFETY: the descriptor is open, `request` is initialized, and the kernel
            // writes at most `capacity` bytes into `buffer`.
            let count = unsafe {
                libc::getattrlistbulk(
                    directory.as_raw_fd(),
                    (&raw mut request).cast::<c_void>(),
                    buffer.as_mut_ptr().cast::<c_void>(),
                    capacity,
                    FSOPT_ATTR_CMN_EXTENDED,
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                return Err(match error.raw_os_error() {
                    Some(libc::ENOTSUP | libc::EINVAL) => io::ErrorKind::Unsupported.into(),
                    _ => error,
                });
            }
            if count == 0 {
                return Ok(());
            }

            // SAFETY: `u64` has no padding, and the bytes stay within the allocation.
            let bytes =
                unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), capacity) };
            let mut at = 0;
            for _ in 0..count {
                let length_bytes = bytes.get(at..at + 4).ok_or_else(malformed)?;
                let length = u32::from_ne_bytes(length_bytes.try_into().map_err(|_| malformed())?);
                let end = at + length as usize;
                parse_entry(path, bytes.get(at..end).ok_or_else(malformed)?, visit)?;
                at = end;
            }
        }
    })
}
