use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::SCHEMA_VERSION;

/// Fields every top-level response carries, wrapped around command-specific data.
///
/// Totals are never reported without these: a number with no completeness or
/// freshness attached is not honest about what it covers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    pub schema_version: u32,
    pub command: String,
    /// When the scan behind this response finished.
    #[schemars(with = "String")]
    pub scanned_at: Timestamp,
    pub scan_completeness: ScanCompleteness,
    pub inaccessible_paths: Vec<InaccessiblePath>,
    pub warnings: Vec<Warning>,
    pub data: T,
}

impl<T> Envelope<T> {
    pub fn new(command: &str, scan: ScanMeta, data: T) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            command: command.to_owned(),
            scanned_at: scan.scanned_at,
            scan_completeness: scan.scan_completeness,
            inaccessible_paths: scan.inaccessible_paths,
            warnings: scan.warnings,
            data,
        }
    }
}

/// The trust metadata a scan produces, before it is attached to a response.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanMeta {
    pub scanned_at: Timestamp,
    pub scan_completeness: ScanCompleteness,
    pub inaccessible_paths: Vec<InaccessiblePath>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompletenessStatus {
    /// Every entry under the root was visited.
    Complete,
    /// Something could not be read, so totals are a lower bound.
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScanCompleteness {
    pub status: CompletenessStatus,
    pub directories_visited: u64,
    pub directories_unreadable: u64,
    pub entries_unreadable: u64,
    /// Directories on another volume, left out of totals. Not a failure.
    pub mount_points_skipped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InaccessiblePath {
    pub path: String,
    pub reason: InaccessibleReason,
    /// OS error text, for humans. Not stable across platforms or locales.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InaccessibleReason {
    PermissionDenied,
    NotFound,
    PathTooLong,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Warning {
    pub code: WarningCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    /// Totals are a lower bound because some paths could not be read.
    IncompleteScan,
    /// macOS privacy protection blocked paths; Full Disk Access would reveal them.
    FullDiskAccessMissing,
    /// Files changed or vanished while being scanned.
    ChangedDuringScan,
    /// Some paths are not valid Unicode and are shown lossily.
    NonUnicodePaths,
    /// Files whose content lives in the cloud take little or no local space.
    CloudPlaceholders,
    /// Files modified since cloning still share blocks that cannot be attributed,
    /// so allocated bytes may overcount.
    DivergedClones,
}

/// Byte and entry totals for a tree.
///
/// Hard links count once, against the lexically first path. `allocated_bytes`
/// also counts blocks shared between APFS clones once where that is measurable.
/// Filesystem metadata such as directory entries is not included.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Totals {
    /// Sum of file lengths. Sparse files and placeholders can far exceed disk use.
    pub logical_bytes: u64,
    /// Bytes this tree occupies on disk.
    pub allocated_bytes: u64,
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    /// Paths that are extra names for a file already counted.
    pub hard_link_duplicates: u64,
    /// Files that share blocks with another file through an APFS clone.
    pub clones: u64,
    /// Files whose data is not stored locally.
    pub cloud_placeholders: u64,
}

/// `data` for `gigsift scan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScanData {
    pub root: String,
    pub totals: Totals,
    /// Largest directories by allocated bytes, recursive. Nested entries overlap.
    pub largest_directories: Vec<SizedEntry>,
}

/// `data` for `gigsift top`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TopData {
    pub root: String,
    pub min_allocated_bytes: u64,
    /// Files and directories at or above the threshold, largest first.
    pub entries: Vec<SizedEntry>,
    /// Entries that met the threshold but fell past the limit.
    pub truncated: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SizedEntry {
    pub path: String,
    pub kind: EntryKind,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
}

/// Replaces the envelope when a command cannot produce a result at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorResponse {
    pub schema_version: u32,
    pub command: String,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    RootNotReadable,
}
