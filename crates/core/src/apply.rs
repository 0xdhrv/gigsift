//! The contract for acting on a plan, and the record it leaves behind.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::BlockReason;

/// `data` for `gigsift clean apply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyData {
    pub run_id: String,
    pub plan_id: String,
    pub results: Vec<ApplyResult>,
    pub moved_count: u64,
    pub skipped_count: u64,
    pub failed_count: u64,
    /// Bytes now in the Trash. Disk space only returns once the Trash is emptied.
    pub moved_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyResult {
    pub action_id: String,
    pub candidate_id: String,
    pub path: String,
    pub outcome: ApplyOutcome,
    /// How the item differs from the plan. Any entry means it was skipped.
    pub changed: Vec<ChangeKind>,
    /// Checks that failed on this run. Any entry means it was skipped.
    pub blocked_by: Vec<BlockReason>,
    pub moved_bytes: u64,
    /// Where it went, when the OS says. Needed to restore it.
    pub trash_location: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApplyOutcome {
    Moved,
    /// Left alone because revalidation objected.
    Skipped,
    /// Left in place because the OS refused or the move failed.
    Failed,
}

/// Ways an item no longer matches what its plan recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Missing,
    /// No longer a real directory, such as one replaced by a symlink.
    NotADirectory,
    /// A different file now sits at this path.
    DifferentFile,
    /// A folder above it was replaced, so the path leads somewhere else.
    ParentChanged,
    Modified,
    SizeChanged,
    /// Its recipe no longer recognizes it, or no longer exists.
    NoLongerRecognized,
    /// The OS would not identify it, so nothing can be compared.
    IdentityUnavailable,
}

/// `gigsift history`. History reports what happened rather than what is on disk,
/// so it carries no scan metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryResponse {
    pub schema_version: u32,
    pub command: String,
    pub data: HistoryData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryData {
    /// Newest first.
    pub actions: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    pub action_id: String,
    pub run_id: String,
    pub plan_id: String,
    #[schemars(with = "String")]
    pub started_at: Timestamp,
    pub path: String,
    pub recipe_id: String,
    pub status: ActionStatus,
    pub changed: Vec<ChangeKind>,
    pub blocked_by: Vec<BlockReason>,
    pub moved_bytes: u64,
    pub trash_location: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    /// Recorded before acting, and never finished. The process stopped midway, so
    /// check the path before assuming either way.
    Interrupted,
    Moved,
    Skipped,
    Failed,
}

impl From<ApplyOutcome> for ActionStatus {
    fn from(outcome: ApplyOutcome) -> Self {
        match outcome {
            ApplyOutcome::Moved => Self::Moved,
            ApplyOutcome::Skipped => Self::Skipped,
            ApplyOutcome::Failed => Self::Failed,
        }
    }
}

/// The record made before an item is touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionStart {
    pub action_id: String,
    pub run_id: String,
    pub plan_id: String,
    pub candidate_id: String,
    pub path: String,
    pub recipe_id: String,
}

pub type AuditError = Box<dyn Error + Send + Sync>;

/// Where apply records what it does. Writing the start must succeed before
/// anything moves: an action that can't be recorded doesn't happen.
pub trait AuditLog {
    fn record_start(&mut self, action: &ActionStart) -> Result<(), AuditError>;
    fn record_result(&mut self, result: &ApplyResult) -> Result<(), AuditError>;
}

/// Moves a path somewhere the user can recover it from.
pub trait Recycler {
    /// Returns where the item went, when the OS says.
    fn recycle(&self, path: &Path) -> Result<Option<PathBuf>, RecycleError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecycleError {
    /// No Trash or Recycle Bin can take this item, such as on a network volume.
    /// The item is left alone: there is deliberately no fallback to deleting it.
    Unavailable(String),
    Failed(String),
}

impl fmt::Display for RecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(detail) => write!(f, "no Trash available here: {detail}"),
            Self::Failed(detail) => write!(f, "could not move to Trash: {detail}"),
        }
    }
}

impl Error for RecycleError {}
