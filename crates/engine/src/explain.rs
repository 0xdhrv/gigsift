use std::path::Path;

use gigsift_core::{
    BlockReason, Envelope, ExplainData, PathKind, ProjectInfo, ProtectedReason, Recognition,
    Safety, SafetyVerdict, protected_reason,
};
use gigsift_recipes::Match;
use gigsift_scanner::{Inspection, RootError, inspect};

use crate::analysis::Analyzer;
use crate::{path_string, resolve};

/// Says what `path` is, what it belongs to, and why it is or isn't safe to touch.
/// Read-only. Never concludes a path is safe to remove: see [`SafetyVerdict`].
pub fn explain(path: &Path, analyzer: &Analyzer) -> Result<Envelope<ExplainData>, RootError> {
    let path = resolve(path);
    let inspection = inspect(&path)?;
    let home = analyzer.home.as_deref();

    let recognized = analyzer.recipes.match_path(&path, home, analyzer.platform);
    // A tool cache has no project, even if a package inside it has a manifest.
    let in_tool_cache = recognized
        .as_ref()
        .is_some_and(|(_, hit)| hit.recipe.detect.paths.is_empty());
    let project_start = match &recognized {
        Some((matched, _)) => matched.parent().unwrap_or(matched),
        None if inspection.kind == PathKind::Directory => &path,
        None => path.parent().unwrap_or(&path),
    };
    let project = if in_tool_cache {
        None
    } else {
        analyzer.project(project_start)
    };

    let protected = if inspection.cloud_placeholder {
        Some(ProtectedReason::CloudPlaceholder)
    } else {
        protected_reason(&path, home)
    };
    let blocked_by = match &recognized {
        Some((matched, hit)) if protected.is_none() => {
            analyzer.check(hit.recipe, matched, project.as_deref())
        }
        _ => Vec::new(),
    };
    let safety = assess(protected, recognized.is_some(), blocked_by);

    let Inspection {
        kind,
        symlink_target,
        logical_bytes,
        allocated_bytes,
        exclusive_bytes,
        hard_links,
        shares_blocks,
        cloud_placeholder,
        meta,
    } = inspection;
    let data = ExplainData {
        path: path_string(&path),
        kind,
        symlink_target: symlink_target.as_deref().map(path_string),
        logical_bytes,
        allocated_bytes,
        exclusive_bytes,
        hard_links,
        shares_blocks,
        cloud_placeholder,
        project: project.map(|project| ProjectInfo {
            root: path_string(&project.root),
            marker: project.marker.clone(),
        }),
        recognized: recognized.map(|(matched, hit)| recognition(&matched, hit)),
        safety,
    };
    Ok(Envelope::new("explain", meta, data))
}

fn assess(
    protected: Option<ProtectedReason>,
    recognized: bool,
    blocked_by: Vec<BlockReason>,
) -> Safety {
    if let Some(reason) = protected {
        return Safety {
            verdict: SafetyVerdict::Protected,
            protected_because: Some(reason),
            summary: protected_summary(reason).to_owned(),
            blocked_by: Vec::new(),
        };
    }
    let (verdict, summary) = match (recognized, blocked_by.is_empty()) {
        (true, false) => (
            SafetyVerdict::Blocked,
            "Recognized, but a safety check failed, so gigsift won't propose it right now.",
        ),
        (true, true) => (
            SafetyVerdict::ReviewRequired,
            "Recognized, and its checks pass right now. Review it in a plan before acting.",
        ),
        (false, _) => (
            SafetyVerdict::Unknown,
            "gigsift does not recognize this. That does not mean it is disposable.",
        ),
    };
    Safety {
        verdict,
        protected_because: None,
        summary: summary.to_owned(),
        blocked_by,
    }
}

fn protected_summary(reason: ProtectedReason) -> &'static str {
    match reason {
        ProtectedReason::System => "Part of the operating system. gigsift will never remove it.",
        ProtectedReason::Credentials => "May hold keys or secrets. gigsift will never remove it.",
        ProtectedReason::GitMetadata => {
            "Git repository data, which may hold unpushed work. gigsift will never remove it."
        }
        ProtectedReason::UserRoot => {
            "A filesystem root or home folder. gigsift will never remove it."
        }
        ProtectedReason::CloudPlaceholder => {
            "Stored in the cloud, so removing it frees no space and could delete the cloud copy."
        }
    }
}

fn recognition(matched: &Path, hit: Match<'_>) -> Recognition {
    let recipe = hit.recipe;
    Recognition {
        recipe_id: recipe.id.clone(),
        name: recipe.name.clone(),
        matched_path: path_string(matched),
        category: recipe.category,
        reason: recipe.explain.reason.clone(),
        loses: recipe.explain.loses.clone(),
        regenerate: hit.regenerate.to_owned(),
        rebuild_cost: recipe.rebuild_cost,
        risk: recipe.risk,
        recommended_action: recipe.action,
    }
}
