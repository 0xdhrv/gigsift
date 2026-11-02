use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Candidate, Category, GitStatus};

/// Why something is not a candidate right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// Project files changed inside the activity window.
    RecentActivity,
    /// A running process is working inside the project.
    RunningProcess,
    UncommittedChanges,
    UnpushedCommits,
    /// Git tracks files here, so it is source.
    GitTrackedFiles,
    /// Holds files git neither tracks nor ignores.
    UntrackedWork,
    /// The recipe needs git to ignore this before calling it output.
    NotIgnoredByGit,
    /// Git could not answer, and a missing answer is not a clean one.
    GitUnavailable,
    /// An application that owns this data is running.
    AppRunning,
    /// The safety policy never allows this path.
    ProtectedPath,
    /// The path can't be carried exactly in JSON, so a plan could not name it.
    NonUnicodePath,
    /// The OS would not identify this path, so apply could not revalidate it.
    IdentityUnavailable,
}

/// `data` for `gigsift projects`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectsData {
    pub root: String,
    /// When set, only projects idle at least this long are listed.
    pub stale_after_seconds: Option<u64>,
    pub projects: Vec<ProjectReport>,
    /// Projects left out by the staleness filter.
    pub hidden_as_recent: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectReport {
    pub root: String,
    pub marker: String,
    pub active: bool,
    /// Every reason the project counts as active. Empty when it is not.
    pub active_because: Vec<BlockReason>,
    #[schemars(with = "Option<String>")]
    pub last_activity: Option<Timestamp>,
    pub git_status: GitStatus,
    pub artifacts: Vec<ArtifactReport>,
    /// Sum across artifacts that nothing blocks.
    pub reclaimable_bytes_estimate: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReport {
    pub path: String,
    pub recipe_id: String,
    pub category: Category,
    pub allocated_bytes: u64,
    pub reclaimable_bytes_estimate: u64,
    pub blocked_by: Vec<BlockReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Build output, dependencies, and tool caches from development work.
    Developer,
}

/// `data` for `gigsift clean plan` and `gigsift clean show`.
///
/// A plan never changes after creation. Applying one must revalidate every item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanData {
    pub plan_id: String,
    pub root: String,
    pub profile: Profile,
    pub target_bytes: u64,
    /// The activity window this plan was built with. Apply reuses it, so a plan
    /// can't pass revalidation under looser rules than it was created with.
    pub active_within_seconds: u64,
    /// Chosen to reach the target, in the order they were picked.
    pub items: Vec<PlanItem>,
    /// Sum of `reclaimable_bytes_estimate` across `items`.
    pub reclaimable_bytes_estimate: u64,
    pub target_met: bool,
    pub shortfall_bytes: u64,
    /// Eligible, but not needed to reach the target.
    pub not_needed: Vec<PlanItem>,
    /// Worth knowing about, but only a guided tool or the user can act on them.
    pub advisories: Vec<Candidate>,
    /// Recognized, but a check failed.
    pub blocked: Vec<BlockedCandidate>,
}

/// `data` for the `find_cleanup_candidates` MCP tool. A survey rather than a plan,
/// so it has no id and nothing is saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CandidatesData {
    pub root: String,
    pub candidates: Vec<Candidate>,
    /// Sum of `reclaimable_bytes_estimate` across `candidates`.
    pub reclaimable_bytes_estimate: u64,
    pub advisories: Vec<Candidate>,
    pub blocked: Vec<BlockedCandidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanItem {
    pub recipe_id: String,
    pub candidate: Candidate,
    pub identity: FileIdentity,
}

/// What must still be true at apply time. Any difference means skip the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub volume: u64,
    pub file: u64,
    #[schemars(with = "Option<String>")]
    pub modified_at: Option<Timestamp>,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlockedCandidate {
    pub path: String,
    pub recipe_id: String,
    pub allocated_bytes: u64,
    pub blocked_by: Vec<BlockReason>,
}
