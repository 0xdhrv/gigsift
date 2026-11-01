//! Acts on a saved plan. This is the only code in gigsift that moves anything.
//!
//! A plan is a claim about the past, so nothing in it is trusted: every item is
//! checked again from scratch, by the same rules that built the plan. Anything that
//! differs is skipped and reported. Items move only through a [`Recycler`], so
//! this module has no way to delete, and each move is logged before it happens.

use std::fs;
use std::io;
use std::path::Path;

use gigsift_core::{
    Action, ActionStart, ApplyData, ApplyOutcome, ApplyResult, AuditLog, BlockReason, ChangeKind,
    Envelope, PlanData, PlanItem, Recycler, ScanMeta,
};
use gigsift_scanner::{Inspection, file_identity, inspect};
use ulid::Ulid;

use crate::analysis::Analyzer;
use crate::discover::{empty_meta, merge};
use crate::{path_string, resolve};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("this analyzer's activity window differs from the plan's")]
    WindowMismatch,
    /// Nothing further moves once the log can't keep up. `moved` says how much
    /// already did, and `history` shows the unfinished action as interrupted.
    #[error("stopped after moving {moved} items, because the audit log failed: {detail}")]
    AuditFailed { moved: u64, detail: String },
}

struct Review {
    changed: Vec<ChangeKind>,
    blocked_by: Vec<BlockReason>,
    exclusive_bytes: u64,
}

impl Review {
    fn stale(change: ChangeKind) -> Self {
        Self {
            changed: vec![change],
            blocked_by: Vec::new(),
            exclusive_bytes: 0,
        }
    }

    fn is_clear(&self) -> bool {
        self.changed.is_empty() && self.blocked_by.is_empty()
    }
}

/// Moves the plan's chosen `items` and nothing else. `not_needed`, `advisories`,
/// and `blocked` are never touched.
pub fn apply_plan(
    plan: &Envelope<PlanData>,
    analyzer: &Analyzer,
    recycler: &dyn Recycler,
    audit: &mut dyn AuditLog,
) -> Result<Envelope<ApplyData>, ApplyError> {
    let plan_data = &plan.data;
    if analyzer.active_within_seconds() != plan_data.active_within_seconds {
        return Err(ApplyError::WindowMismatch);
    }

    let run_id = format!("run_{}", Ulid::new());
    let mut meta = empty_meta();
    let mut data = ApplyData {
        run_id: run_id.clone(),
        plan_id: plan_data.plan_id.clone(),
        results: Vec::new(),
        moved_count: 0,
        skipped_count: 0,
        failed_count: 0,
        moved_bytes: 0,
    };

    for item in &plan_data.items {
        let action_id = format!("act_{}", Ulid::new());
        let audit_failed = |detail: String, moved| ApplyError::AuditFailed { moved, detail };
        let start = ActionStart {
            action_id: action_id.clone(),
            run_id: run_id.clone(),
            plan_id: plan_data.plan_id.clone(),
            candidate_id: item.candidate.candidate_id.clone(),
            path: item.candidate.path.clone(),
            recipe_id: item.recipe_id.clone(),
        };
        audit
            .record_start(&start)
            .map_err(|error| audit_failed(error.to_string(), data.moved_count))?;

        let review = review(item, analyzer, &mut meta);
        let result = if review.is_clear() {
            act(item, action_id, &review, recycler)
        } else {
            skipped(item, action_id, review)
        };

        match result.outcome {
            ApplyOutcome::Moved => {
                data.moved_count += 1;
                data.moved_bytes += result.moved_bytes;
            }
            ApplyOutcome::Skipped => data.skipped_count += 1,
            ApplyOutcome::Failed => data.failed_count += 1,
        }
        let recorded = audit.record_result(&result);
        data.results.push(result);
        recorded.map_err(|error| audit_failed(error.to_string(), data.moved_count))?;
    }
    Ok(Envelope::new("clean apply", meta, data))
}

fn act(
    item: &PlanItem,
    action_id: String,
    review: &Review,
    recycler: &dyn Recycler,
) -> ApplyResult {
    let mut result = ApplyResult {
        action_id,
        candidate_id: item.candidate.candidate_id.clone(),
        path: item.candidate.path.clone(),
        outcome: ApplyOutcome::Moved,
        changed: Vec::new(),
        blocked_by: Vec::new(),
        moved_bytes: review.exclusive_bytes,
        trash_location: None,
        error: None,
    };
    match recycler.recycle(Path::new(&item.candidate.path)) {
        Ok(location) => result.trash_location = location.as_deref().map(path_string),
        Err(error) => {
            result.outcome = ApplyOutcome::Failed;
            result.moved_bytes = 0;
            result.error = Some(error.to_string());
        }
    }
    result
}

fn skipped(item: &PlanItem, action_id: String, review: Review) -> ApplyResult {
    ApplyResult {
        action_id,
        candidate_id: item.candidate.candidate_id.clone(),
        path: item.candidate.path.clone(),
        outcome: ApplyOutcome::Skipped,
        changed: review.changed,
        blocked_by: review.blocked_by,
        moved_bytes: 0,
        trash_location: None,
        error: None,
    }
}

/// Compares an item with the world as it is now. The first test that can't pass
/// ends the review, since later ones would only be comparing something else.
fn review(item: &PlanItem, analyzer: &Analyzer, meta: &mut ScanMeta) -> Review {
    let path = Path::new(&item.candidate.path);
    match fs::symlink_metadata(path) {
        Ok(found) if found.is_dir() => {}
        Ok(_) => return Review::stale(ChangeKind::NotADirectory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Review::stale(ChangeKind::Missing);
        }
        Err(_) => return Review::stale(ChangeKind::IdentityUnavailable),
    }
    if resolve(path) != path {
        return Review::stale(ChangeKind::ParentChanged);
    }
    let Ok(size) = inspect(path) else {
        return Review::stale(ChangeKind::IdentityUnavailable);
    };
    merge(meta, &size.meta);

    let changed = compare(item, path, &size);
    if !changed.is_empty() {
        return Review {
            changed,
            blocked_by: Vec::new(),
            exclusive_bytes: 0,
        };
    }

    let Some(recipe) = current_recipe(item, path, analyzer) else {
        return Review::stale(ChangeKind::NoLongerRecognized);
    };
    let project = path
        .parent()
        .filter(|_| !recipe.detect.paths.is_empty())
        .and_then(|parent| analyzer.project(parent));
    Review {
        changed: Vec::new(),
        blocked_by: analyzer.check(recipe, path, project.as_deref()),
        exclusive_bytes: size.exclusive_bytes,
    }
}

fn compare(item: &PlanItem, path: &Path, size: &Inspection) -> Vec<ChangeKind> {
    let Some(now) = file_identity(path, size.allocated_bytes) else {
        return vec![ChangeKind::IdentityUnavailable];
    };
    let then = &item.identity;
    if (now.volume, now.file) != (then.volume, then.file) {
        return vec![ChangeKind::DifferentFile];
    }
    let mut changed = Vec::new();
    if now.modified_at != then.modified_at {
        changed.push(ChangeKind::Modified);
    }
    let same_size = now.allocated_bytes == then.allocated_bytes
        && size.logical_bytes == item.candidate.logical_bytes;
    if !same_size {
        changed.push(ChangeKind::SizeChanged);
    }
    changed
}

/// The recipe, if it still recognizes exactly this path and still allows moving
/// it. A plan edited by hand fails here, whatever it claims.
fn current_recipe<'a>(
    item: &PlanItem,
    path: &Path,
    analyzer: &Analyzer<'a>,
) -> Option<&'a gigsift_recipes::Recipe> {
    let home = analyzer.home.as_deref();
    let (matched, hit) = analyzer.recipes.match_path(path, home, analyzer.platform)?;
    let still_valid = matched == path
        && hit.recipe.id == item.recipe_id
        && hit.recipe.action == Action::MoveToTrash;
    still_valid.then_some(hit.recipe)
}
