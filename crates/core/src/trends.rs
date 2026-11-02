//! Storage over time: what was recorded, what changed, and where it's heading.

use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A volume's capacity at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeSpace {
    pub volume: u64,
    pub total_bytes: u64,
    /// Space available to the user. Purgeable space on APFS is not counted.
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySize {
    pub path: String,
    pub allocated_bytes: u64,
}

/// One recorded scan. Only folders above a size floor are kept, which is what
/// makes a year of daily snapshots cheap to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub snapshot_id: String,
    pub root: String,
    pub taken_at: Timestamp,
    pub complete: bool,
    pub allocated_bytes: u64,
    pub logical_bytes: u64,
    pub files: u64,
    pub volume: Option<VolumeSpace>,
    /// The smallest folder size this snapshot kept.
    pub min_directory_bytes: u64,
    pub directories: Vec<DirectorySize>,
}

/// `data` for `gigsift snapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotData {
    pub snapshot_id: String,
    pub root: String,
    pub allocated_bytes: u64,
    pub logical_bytes: u64,
    pub files: u64,
    pub directories_recorded: u64,
    pub volume: Option<VolumeSpace>,
}

/// `data` for `gigsift changes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangesData {
    pub root: String,
    pub since_seconds: u64,
    /// The snapshot compared against. Absent when none exists for this folder.
    pub baseline: Option<Baseline>,
    /// Positive means the tree grew. Absent without a baseline.
    pub allocated_delta_bytes: Option<i64>,
    /// Largest first, each attributed to the deepest folder that explains it.
    pub changes: Vec<PathChange>,
    /// Changes that fell past the limit.
    pub truncated: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub snapshot_id: String,
    #[schemars(with = "String")]
    pub taken_at: Timestamp,
    /// False when the baseline scan couldn't read everything.
    pub complete: bool,
    /// True when no snapshot is as old as asked, so this covers a shorter period.
    pub newer_than_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathChange {
    pub path: String,
    /// Absent when the folder wasn't recorded, because it was missing or small.
    pub previous_bytes: Option<u64>,
    /// Absent when the folder is gone or now below the recorded floor.
    pub current_bytes: Option<u64>,
    pub delta_bytes: i64,
    /// True when one side is unknown, so the delta could be off by up to the floor.
    pub approximate: bool,
}

/// One point on a volume's free-space history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceSample {
    pub taken_at: Timestamp,
    pub available_bytes: u64,
}

/// `gigsift forecast`. A forecast reads recorded history rather than scanning, so
/// like history it carries no scan metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForecastResponse {
    pub schema_version: u32,
    pub command: String,
    pub data: ForecastData,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForecastData {
    pub volume: VolumeSpace,
    pub samples: u64,
    pub span_days: f64,
    pub outlook: Outlook,
    /// Positive means free space is shrinking. Absent with too little history.
    pub bytes_used_per_day: Option<f64>,
    /// Best estimate. Set only when `outlook` is `filling`.
    pub days_until_full: Option<f64>,
    /// Earliest plausible, at 80% confidence.
    pub days_until_full_low: Option<f64>,
    /// Latest plausible, at 80% confidence. Absent when the trend is so noisy
    /// that the disk might not fill at all.
    pub days_until_full_high: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outlook {
    /// Too few snapshots, or too short a period, to say anything.
    InsufficientData,
    /// Free space is holding steady or growing.
    NotFilling,
    Filling,
}
