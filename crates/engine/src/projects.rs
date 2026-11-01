use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use gigsift_core::{ArtifactReport, Envelope, ProjectReport, ProjectsData};
use gigsift_scanner::RootError;

use crate::analysis::{Analyzer, ProjectContext};
use crate::discover::{Artifact, survey};
use crate::{path_string, resolve};

/// Lists projects that hold recognized artifacts, biggest payoff first. A project
/// with nothing to reclaim isn't listed, and tool caches belong to no project.
pub fn find_projects(
    root: &Path,
    stale_after_seconds: Option<u64>,
    analyzer: &Analyzer,
) -> Result<Envelope<ProjectsData>, RootError> {
    let root = resolve(root);
    let survey = survey(&root, analyzer)?;

    let mut grouped: BTreeMap<String, (Rc<ProjectContext>, Vec<&Artifact>)> = BTreeMap::new();
    for artifact in &survey.artifacts {
        if let Some(project) = &artifact.project {
            grouped
                .entry(path_string(&project.root))
                .or_insert_with(|| (Rc::clone(project), Vec::new()))
                .1
                .push(artifact);
        }
    }

    let total = grouped.len() as u64;
    let is_stale = |project: &ProjectContext| {
        stale_after_seconds.is_none_or(|age| analyzer.is_idle_for(project.last_activity, age))
    };
    let mut projects: Vec<ProjectReport> = grouped
        .into_values()
        .filter(|(project, _)| is_stale(project))
        .map(|(project, artifacts)| report(&project, &artifacts))
        .collect();
    projects.sort_by(|a, b| {
        b.reclaimable_bytes_estimate
            .cmp(&a.reclaimable_bytes_estimate)
            .then_with(|| a.root.cmp(&b.root))
    });

    let data = ProjectsData {
        root: path_string(&root),
        stale_after_seconds,
        hidden_as_recent: total - projects.len() as u64,
        projects,
    };
    Ok(Envelope::new("projects", survey.meta, data))
}

fn report(project: &ProjectContext, artifacts: &[&Artifact]) -> ProjectReport {
    let artifacts: Vec<ArtifactReport> = artifacts
        .iter()
        .map(|artifact| ArtifactReport {
            path: path_string(&artifact.path),
            recipe_id: artifact.hit.recipe.id.clone(),
            category: artifact.hit.recipe.category,
            allocated_bytes: artifact.size.allocated_bytes,
            reclaimable_bytes_estimate: artifact.size.exclusive_bytes,
            blocked_by: artifact.blocked_by.clone(),
        })
        .collect();
    let reclaimable_bytes_estimate = artifacts
        .iter()
        .filter(|artifact| artifact.blocked_by.is_empty())
        .map(|artifact| artifact.reclaimable_bytes_estimate)
        .sum();
    ProjectReport {
        root: path_string(&project.root),
        marker: project.marker.clone(),
        active: project.active(),
        active_because: project.active_because.clone(),
        last_activity: project.last_activity,
        git_status: project.git_status,
        artifacts,
        reclaimable_bytes_estimate,
    }
}
