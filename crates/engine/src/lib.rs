//! Operations that combine the scanner, recipes, and safety policy. Every surface
//! calls these, so a rule enforced here holds for the CLI, MCP, and Raycast alike.

use std::fs;
use std::path::{Path, PathBuf};

use gigsift_core::{
    Envelope, ExplainData, PathKind, ProjectInfo, ProtectedReason, Protection, Recognition, Safety,
    SafetyVerdict, protected_reason,
};
use gigsift_recipes::{Match, Platform, RecipeSet};
use gigsift_scanner::{Inspection, RootError, inspect};

/// Checked in order, so a repository root beats a manifest in the same folder.
const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
];

/// Says what `path` is, what it belongs to, and why it is or isn't safe to touch.
/// Read-only. Never concludes a path is safe to remove: see [`SafetyVerdict`].
pub fn explain(path: &Path, recipes: &RecipeSet) -> Result<Envelope<ExplainData>, RootError> {
    let path = resolve(path);
    let inspection = inspect(&path)?;
    let home = std::env::home_dir();

    let recognized = recipes.match_path(&path, Platform::current());
    let project_start = match &recognized {
        Some((matched, _)) => matched.parent().unwrap_or(matched).to_owned(),
        None if inspection.kind == PathKind::Directory => path.clone(),
        None => path.parent().unwrap_or(&path).to_owned(),
    };

    let reason = if inspection.cloud_placeholder {
        Some(ProtectedReason::CloudPlaceholder)
    } else {
        protected_reason(&path, home.as_deref())
    };
    let pending = recognized
        .as_ref()
        .map_or(&[][..], |(_, hit)| &hit.recipe.protect.when);
    let safety = assess(reason, recognized.is_some(), pending);

    let Inspection {
        kind,
        symlink_target,
        logical_bytes,
        allocated_bytes,
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
        hard_links,
        shares_blocks,
        cloud_placeholder,
        project: find_project(&project_start, home.as_deref()),
        recognized: recognized.map(|(matched, hit)| recognition(&matched, hit)),
        safety,
    };
    Ok(Envelope::new("explain", meta, data))
}

/// Resolves everything above the final name, so a symlink is itself described
/// while `/tmp/../etc` and symlinked parents can't dodge the safety checks.
fn resolve(path: &Path) -> PathBuf {
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

fn assess(protected: Option<ProtectedReason>, recognized: bool, pending: &[Protection]) -> Safety {
    if let Some(reason) = protected {
        return Safety {
            verdict: SafetyVerdict::Protected,
            protected_because: Some(reason),
            summary: protected_summary(reason).to_owned(),
            checks_pending: Vec::new(),
        };
    }
    let (verdict, summary) = if recognized {
        (
            SafetyVerdict::ReviewRequired,
            "Recognized, but its safety checks have not run. Do not treat it as safe to remove.",
        )
    } else {
        (
            SafetyVerdict::Unknown,
            "gigsift does not recognize this. That does not mean it is disposable.",
        )
    };
    Safety {
        verdict,
        protected_because: None,
        summary: summary.to_owned(),
        checks_pending: pending.to_vec(),
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

/// The nearest folder with a project marker. The search stops at home: a stray
/// `package.json` there does not make everything under it one project.
fn find_project(start: &Path, home: Option<&Path>) -> Option<ProjectInfo> {
    let within = start
        .ancestors()
        .take_while(|dir| home != Some(*dir))
        .collect::<Vec<_>>();
    within.into_iter().find_map(|dir| {
        let marker = PROJECT_MARKERS.iter().find(|m| dir.join(m).exists())?;
        Some(ProjectInfo {
            root: path_string(dir),
            marker: (*marker).to_owned(),
        })
    })
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

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
