use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Action, Category, ProtectedReason, Protection, RebuildCost, Risk, SCHEMA_VERSION};

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

/// `data` for `gigsift explain`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplainData {
    /// Absolute, with symlinks in parent folders resolved.
    pub path: String,
    pub kind: PathKind,
    /// Set for symlinks, which are described but never followed.
    pub symlink_target: Option<String>,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    /// Above 1, removing this path frees nothing until the other names go too.
    pub hard_links: u32,
    /// Some of this storage is shared with an APFS clone, so removal frees less.
    pub shares_blocks: bool,
    pub cloud_placeholder: bool,
    /// The nearest project around this path.
    pub project: Option<ProjectInfo>,
    /// Absent when no recipe knows this path. That is not evidence it is junk.
    pub recognized: Option<Recognition>,
    pub safety: Safety,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectInfo {
    pub root: String,
    /// The file or folder that identified it, such as `Cargo.toml`.
    pub marker: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Recognition {
    pub recipe_id: String,
    pub name: String,
    /// The recognized directory, which may be a parent of `path`.
    pub matched_path: String,
    pub category: Category,
    pub reason: String,
    pub loses: String,
    /// Guidance for a human. gigsift never executes this text.
    pub regenerate: String,
    pub rebuild_cost: RebuildCost,
    pub risk: Risk,
    pub recommended_action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Safety {
    pub verdict: SafetyVerdict,
    /// Set exactly when the verdict is `protected`.
    pub protected_because: Option<ProtectedReason>,
    pub summary: String,
    /// Conditions that must be verified before this could become a candidate.
    pub checks_pending: Vec<Protection>,
}

/// There is deliberately no `safe` verdict: only a plan, revalidated at apply
/// time, can establish that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SafetyVerdict {
    /// Never a cleanup candidate.
    Protected,
    /// A recipe knows this path, but its checks have not run.
    ReviewRequired,
    /// gigsift does not know what this is. Do not assume it is disposable.
    Unknown,
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
