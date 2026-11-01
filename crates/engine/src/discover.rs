//! Finds everything recipes recognize under a root, then checks and sizes it.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gigsift_core::{
    BlockReason, CompletenessStatus, FileIdentity, ScanCompleteness, ScanMeta, Warning,
};
use gigsift_recipes::Match;
use gigsift_scanner::{Inspection, RootError, file_identity, inspect};
use jiff::Timestamp;
use rayon::prelude::*;

use crate::analysis::{Analyzer, ProjectContext};

/// Folders under home that hold no projects. Dot-folders are skipped too: that is
/// where tools install themselves, and caches there are found by exact location.
const HOME_SKIP: &[&str] = &["AppData", "Library", "Movies", "Music", "Pictures"];

pub(crate) struct Artifact<'a> {
    pub path: PathBuf,
    pub hit: Match<'a>,
    pub project: Option<Rc<ProjectContext>>,
    pub blocked_by: Vec<BlockReason>,
    pub size: Inspection,
    pub identity: Option<FileIdentity>,
}

pub(crate) struct Survey<'a> {
    pub artifacts: Vec<Artifact<'a>>,
    pub meta: ScanMeta,
}

pub(crate) fn survey<'a>(root: &Path, analyzer: &Analyzer<'a>) -> Result<Survey<'a>, RootError> {
    fs::read_dir(root).map_err(|error| RootError {
        path: root.to_owned(),
        message: error.to_string(),
    })?;

    let home = analyzer.home.as_deref();
    let skip = home.map(home_skip_list).unwrap_or_default();
    let mut found = analyzer.recipes.find(root, analyzer.platform, &skip);
    if let Some(home) = home {
        let caches = analyzer.recipes.home_matches(home, analyzer.platform);
        found.extend(
            caches
                .into_iter()
                .filter(|(path, _)| path.starts_with(root)),
        );
    }

    // Checks share cached project state, so only sizing runs in parallel.
    let sizes: Vec<_> = found.par_iter().map(|(path, _)| inspect(path)).collect();

    let mut artifacts = Vec::new();
    let mut meta = empty_meta();
    for ((path, hit), size) in found.into_iter().zip(sizes) {
        // Gone since the walk found it, which is not worth failing the run over.
        let Ok(size) = size else {
            continue;
        };
        merge(&mut meta, &size.meta);
        let is_project_artifact = !hit.recipe.detect.paths.is_empty();
        let project = path
            .parent()
            .filter(|_| is_project_artifact)
            .and_then(|parent| analyzer.project(parent));
        let mut blocked_by = analyzer.check(hit.recipe, &path, project.as_deref());
        let identity = file_identity(&path, size.allocated_bytes);
        if identity.is_none() {
            blocked_by.push(BlockReason::IdentityUnavailable);
        }
        artifacts.push(Artifact {
            path,
            hit,
            project,
            blocked_by,
            size,
            identity,
        });
    }
    Ok(Survey { artifacts, meta })
}

fn home_skip_list(home: &Path) -> Vec<PathBuf> {
    let dot_folders = fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path());
    let named = HOME_SKIP.iter().map(|name| home.join(name));
    dot_folders.chain(named).collect()
}

pub(crate) fn empty_meta() -> ScanMeta {
    ScanMeta {
        scanned_at: Timestamp::now(),
        scan_completeness: ScanCompleteness {
            status: CompletenessStatus::Complete,
            directories_visited: 0,
            directories_unreadable: 0,
            entries_unreadable: 0,
            mount_points_skipped: 0,
        },
        inaccessible_paths: Vec::new(),
        warnings: Vec::new(),
    }
}

/// Each artifact gets its own scan. The run is only as complete as its worst one.
pub(crate) fn merge(total: &mut ScanMeta, scan: &ScanMeta) {
    let completeness = &mut total.scan_completeness;
    completeness.directories_visited += scan.scan_completeness.directories_visited;
    completeness.directories_unreadable += scan.scan_completeness.directories_unreadable;
    completeness.entries_unreadable += scan.scan_completeness.entries_unreadable;
    completeness.mount_points_skipped += scan.scan_completeness.mount_points_skipped;
    if scan.scan_completeness.status == CompletenessStatus::Partial {
        completeness.status = CompletenessStatus::Partial;
    }
    total
        .inaccessible_paths
        .extend(scan.inaccessible_paths.iter().cloned());
    for warning in &scan.warnings {
        if !has_code(&total.warnings, warning) {
            total.warnings.push(warning.clone());
        }
    }
    total.scanned_at = total.scanned_at.max(scan.scanned_at);
}

fn has_code(warnings: &[Warning], warning: &Warning) -> bool {
    warnings.iter().any(|known| known.code == warning.code)
}
