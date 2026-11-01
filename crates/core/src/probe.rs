//! What a platform crate reports about one file. Native facts only: policy and
//! accounting live in the scanner, so both platforms share one set of rules.

/// Identity of a file on disk. Two paths with the same id are hard links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    pub volume: u64,
    pub file: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileProbe {
    pub allocated_bytes: u64,
    /// `None` when the OS would not say; such a file is treated as unique.
    pub id: Option<FileId>,
    pub links: u32,
    /// The file's data is not stored locally.
    pub placeholder: bool,
    /// Set only when the file may share blocks with another file.
    pub clone: Option<CloneInfo>,
}

/// Block sharing as reported by APFS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloneInfo {
    /// Identical clones share this. A clone gets a new id once modified, while
    /// still sharing whatever blocks it did not rewrite.
    pub id: u64,
    /// Bytes only this file owns.
    pub private_bytes: u64,
    /// Live clones with this id, including ones outside the scanned tree.
    pub references: u32,
}
