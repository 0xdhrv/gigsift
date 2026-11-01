use jiff::Timestamp;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A single item a cleanup plan proposes. Producing one never touches the disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub schema_version: u32,
    pub candidate_id: String,
    pub path: String,
    pub category: Category,
    /// Absent for items that belong to no project, such as a global cache.
    pub project: Option<ProjectRef>,
    pub git_status: GitStatus,
    pub logical_bytes: u64,
    pub allocated_bytes: u64,
    /// What removal is expected to free; hard links and clones can make this
    /// smaller than `allocated_bytes`.
    pub reclaimable_bytes_estimate: u64,
    pub reason: String,
    pub loses: String,
    /// Guidance for a human. gigsift never executes this text.
    pub regenerate: String,
    pub rebuild_cost: RebuildCost,
    pub risk: Risk,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub confidence: f64,
    pub recommended_action: Action,
    pub requires_confirmation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectRef {
    pub root: String,
    pub active: bool,
    #[schemars(with = "Option<String>")]
    pub last_activity: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    DeveloperDependency,
    BuildArtifact,
    PythonEnvironment,
    PackageCache,
    ToolCache,
    ModelCache,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GitStatus {
    Clean,
    Dirty,
    Untracked,
    UnpushedCommits,
    NotARepository,
    /// Git could not be consulted. Treated as unsafe, never as clean.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RebuildCost {
    pub time: RebuildTime,
    pub network: NetworkCost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RebuildTime {
    Seconds,
    Minutes,
    Hours,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NetworkCost {
    None,
    Low,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// The fixed allow-list of things a recipe may recommend. There is deliberately
/// no variant that carries a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    MoveToTrash,
    ReportOnly,
    GuidedTool,
}

/// Conditions that veto a candidate. Recipes declare them; the planner checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Protection {
    InsideActiveProject,
    /// Git tracks something in here, so it is source, whatever its name says.
    ContainsGitTrackedFiles,
    /// Untracked and not ignored. Never assumed to be disposable.
    ContainsUntrackedWork,
}
