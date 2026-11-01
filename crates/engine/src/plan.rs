//! Builds a cleanup plan. This only reads: saving the plan is a separate step, and
//! nothing here can move or delete anything.

use std::cmp::Reverse;
use std::path::Path;

use gigsift_core::{
    Action, BlockedCandidate, Candidate, Envelope, GitStatus, PlanData, PlanItem, Profile,
    ProjectRef, Risk, SCHEMA_VERSION,
};
use gigsift_scanner::RootError;
use ulid::Ulid;

use crate::analysis::Analyzer;
use crate::discover::{Artifact, survey};
use crate::{path_string, resolve};

/// Picks enough to reach `target_bytes`, lowest risk first. Items git can vouch for
/// come before those it can't, then largest first, so the target takes as few
/// items and rebuilds as possible. Ties go to the project idle longest. Tool
/// caches come last, since refilling one costs every project a download.
pub fn create_plan(
    root: &Path,
    target_bytes: u64,
    profile: Profile,
    analyzer: &Analyzer,
) -> Result<Envelope<PlanData>, RootError> {
    let root = resolve(root);
    let survey = survey(&root, analyzer)?;

    let mut eligible = Vec::new();
    let mut advisories = Vec::new();
    let mut blocked = Vec::new();
    for artifact in &survey.artifacts {
        if !artifact.blocked_by.is_empty() {
            blocked.push(BlockedCandidate {
                path: path_string(&artifact.path),
                recipe_id: artifact.hit.recipe.id.clone(),
                allocated_bytes: artifact.size.allocated_bytes,
                blocked_by: artifact.blocked_by.clone(),
            });
        } else if artifact.hit.recipe.action == Action::MoveToTrash {
            eligible.push(artifact);
        } else {
            advisories.push(candidate(artifact));
        }
    }
    eligible.sort_by_key(|artifact| {
        let recipe = artifact.hit.recipe;
        let project = artifact.project.as_ref();
        let last_activity = project.and_then(|p| p.last_activity);
        let unverified = project.is_some_and(|p| p.git_status == GitStatus::NotARepository);
        (
            project.is_none(),
            risk_rank(recipe.risk),
            unverified,
            Reverse(artifact.size.exclusive_bytes),
            last_activity,
        )
    });

    let mut items = Vec::new();
    let mut not_needed = Vec::new();
    let mut reclaimable = 0u64;
    for artifact in eligible {
        // `survey` blocks anything without an identity, so this always exists.
        let Some(identity) = artifact.identity else {
            continue;
        };
        let item = PlanItem {
            recipe_id: artifact.hit.recipe.id.clone(),
            candidate: candidate(artifact),
            identity,
        };
        let frees_nothing = artifact.size.exclusive_bytes == 0;
        if reclaimable >= target_bytes || frees_nothing {
            not_needed.push(item);
        } else {
            reclaimable += artifact.size.exclusive_bytes;
            items.push(item);
        }
    }

    blocked.sort_by_key(|blocked| Reverse(blocked.allocated_bytes));
    let data = PlanData {
        plan_id: format!("plan_{}", Ulid::new()),
        root: path_string(&root),
        profile,
        target_bytes,
        active_within_seconds: analyzer.active_within_seconds(),
        items,
        reclaimable_bytes_estimate: reclaimable,
        target_met: reclaimable >= target_bytes,
        shortfall_bytes: target_bytes.saturating_sub(reclaimable),
        not_needed,
        advisories,
        blocked,
    };
    Ok(Envelope::new("clean plan", survey.meta, data))
}

fn risk_rank(risk: Risk) -> u8 {
    match risk {
        Risk::Low => 0,
        Risk::Medium => 1,
        Risk::High => 2,
    }
}

fn candidate(artifact: &Artifact) -> Candidate {
    let recipe = artifact.hit.recipe;
    let project = artifact.project.as_ref();
    let git_status = project.map_or(GitStatus::NotARepository, |p| p.git_status);
    Candidate {
        schema_version: SCHEMA_VERSION,
        candidate_id: format!("cand_{}", Ulid::new()),
        path: path_string(&artifact.path),
        category: recipe.category,
        project: project.map(|project| ProjectRef {
            root: path_string(&project.root),
            active: project.active(),
            last_activity: project.last_activity,
        }),
        git_status,
        logical_bytes: artifact.size.logical_bytes,
        allocated_bytes: artifact.size.allocated_bytes,
        reclaimable_bytes_estimate: artifact.size.exclusive_bytes,
        reason: recipe.explain.reason.clone(),
        loses: recipe.explain.loses.clone(),
        regenerate: artifact.hit.regenerate.to_owned(),
        rebuild_cost: recipe.rebuild_cost,
        risk: recipe.risk,
        confidence: confidence(recipe.risk, project.is_some(), git_status),
        recommended_action: recipe.action,
        requires_confirmation: true,
    }
}

/// How sure gigsift is that this is what the recipe says it is. A project with a
/// clean repo has the most evidence; without git, there is less to go on.
fn confidence(risk: Risk, in_project: bool, git_status: GitStatus) -> f64 {
    let base = match risk {
        Risk::Low => 0.95,
        Risk::Medium => 0.8,
        Risk::High => 0.6,
    };
    if in_project && git_status == GitStatus::NotARepository {
        base - 0.1
    } else {
        base
    }
}
