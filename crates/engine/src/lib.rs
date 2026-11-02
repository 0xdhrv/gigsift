//! Operations that combine the scanner, recipes, and safety policy. Every surface
//! calls these, so a rule enforced here holds for the CLI, MCP, and Raycast alike.

pub mod activity;
pub mod analysis;
mod apply;
mod changes;
mod discover;
mod explain;
mod forecast;
pub mod git;
mod plan;
mod projects;
mod storage;

use std::fs;
use std::path::{Path, PathBuf};

pub use analysis::{Analyzer, ProjectContext};
pub use apply::{ApplyError, apply_plan};
pub use changes::{
    SNAPSHOT_MIN_DIRECTORY_BYTES, attribute, compare_with_snapshot, snapshot_key, take_snapshot,
};
pub use explain::explain;
pub use forecast::forecast;
pub use plan::{create_plan, find_candidates};
pub use projects::find_projects;
pub use storage::{largest_entries, storage_summary};

/// Resolves everything above the final name, so a symlink is itself described
/// while `/tmp/../etc` and symlinked parents can't dodge the safety checks.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let resolved = match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => fs::canonicalize(parent).map(|parent| parent.join(name)),
        _ => fs::canonicalize(&absolute),
    };
    strip_verbatim_prefix(resolved.unwrap_or(absolute))
}

/// `canonicalize` on Windows returns `\\?\C:\...`, which no user would recognize.
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let stripped = path.to_str().and_then(|text| text.strip_prefix(r"\\?\"));
    match stripped {
        Some(text) if !text.starts_with("UNC") => PathBuf::from(text),
        _ => path,
    }
}

pub(crate) fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
