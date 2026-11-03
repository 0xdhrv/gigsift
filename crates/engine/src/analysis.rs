//! What gigsift knows about a project, and the checks that veto a candidate.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use gigsift_core::{BlockReason, GitStatus, Protection, protected_reason};
use gigsift_recipes::{Platform, Recipe, RecipeSet};
use gigsift_scanner::newest_change;
use jiff::{SignedDuration, Timestamp};
use rayon::prelude::*;

use crate::activity::RunningProcesses;
use crate::git::{self, Repo};

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

/// Everything the checks need, gathered once per run so that every answer in a
/// plan describes the same moment.
pub struct Analyzer<'a> {
    pub(crate) recipes: &'a RecipeSet,
    pub(crate) home: Option<PathBuf>,
    pub(crate) platform: Platform,
    processes: RunningProcesses,
    active_within_seconds: u64,
    now: Timestamp,
    repos: Mutex<HashMap<PathBuf, RepoLookup>>,
    projects: Mutex<HashMap<PathBuf, Arc<ProjectContext>>>,
}

type RepoLookup = Result<Option<Repo>, git::GitUnavailable>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectContext {
    pub root: PathBuf,
    pub marker: String,
    pub repo: Option<Repo>,
    pub git_status: GitStatus,
    pub last_activity: Option<Timestamp>,
    pub active_because: Vec<BlockReason>,
}

impl ProjectContext {
    pub fn active(&self) -> bool {
        !self.active_because.is_empty()
    }
}

impl<'a> Analyzer<'a> {
    pub fn new(
        recipes: &'a RecipeSet,
        home: Option<PathBuf>,
        processes: RunningProcesses,
        active_within_seconds: u64,
    ) -> Self {
        Self {
            recipes,
            home,
            platform: Platform::current(),
            processes,
            active_within_seconds,
            now: Timestamp::now(),
            repos: Mutex::default(),
            projects: Mutex::default(),
        }
    }

    /// An analyzer for the current user and the machine as it is now.
    pub fn for_user(recipes: &'a RecipeSet, active_within_seconds: u64) -> Self {
        let home = std::env::home_dir().and_then(|home| home.canonicalize().ok());
        Self::new(
            recipes,
            home,
            RunningProcesses::snapshot(),
            active_within_seconds,
        )
    }

    pub(crate) fn active_within_seconds(&self) -> u64 {
        self.active_within_seconds
    }

    /// The project around `start`, if any. The search stops at home: a stray
    /// `package.json` there doesn't make everything under it one project.
    pub fn project(&self, start: &Path) -> Option<Arc<ProjectContext>> {
        let (dir, marker) = self.nearest_marker(start)?;
        Some(self.context(dir, marker))
    }

    /// Gathers every project around `starts` in parallel, so that later calls to
    /// [`Self::project`] only look them up. Git and the walks are most of a survey.
    pub(crate) fn load_projects(&self, starts: &[&Path]) {
        let markers: HashSet<_> = starts
            .par_iter()
            .filter_map(|start| self.nearest_marker(start))
            .collect();
        let markers: Vec<_> = markers.into_iter().collect();
        markers.par_iter().for_each(|(dir, _)| {
            let _ = self.repo_lookup(dir);
        });

        // One folder per root, or two threads could gather the same project.
        let mut roots = HashSet::new();
        let unique: Vec<_> = markers
            .into_iter()
            .filter(|(dir, _)| {
                let lookup = self.repo_lookup(dir);
                let repo = self.usable_repo(&lookup);
                roots.insert(repo.map_or(dir.to_path_buf(), |repo| repo.top))
            })
            .collect();
        unique.par_iter().for_each(|(dir, marker)| {
            self.context(dir, marker);
        });
    }

    fn context(&self, dir: &Path, marker: &str) -> Arc<ProjectContext> {
        let lookup = self.repo_lookup(dir);
        let repo = self.usable_repo(&lookup);
        let root = repo.as_ref().map_or(dir, |repo| repo.top.as_path());
        if let Some(known) = lock(&self.projects).get(root) {
            return Arc::clone(known);
        }

        let marker = if repo.is_some() { ".git" } else { marker };
        let context = self.gather(root, marker, repo.clone(), lookup.is_err());
        let context = Arc::new(context);
        lock(&self.projects).insert(root.to_owned(), Arc::clone(&context));
        context
    }

    fn repo_lookup(&self, dir: &Path) -> RepoLookup {
        if let Some(known) = lock(&self.repos).get(dir) {
            return known.clone();
        }
        let lookup = git::find_repo(dir);
        lock(&self.repos).insert(dir.to_owned(), lookup.clone());
        lookup
    }

    /// A home folder kept in git says nothing about the projects under it.
    fn usable_repo(&self, lookup: &RepoLookup) -> Option<Repo> {
        let repo = lookup.clone().ok().flatten()?;
        (self.home.as_deref() != Some(repo.top.as_path())).then_some(repo)
    }

    fn nearest_marker<'p>(&self, start: &'p Path) -> Option<(&'p Path, &'static str)> {
        start
            .ancestors()
            .take_while(|dir| self.home.as_deref() != Some(*dir))
            .find_map(|dir| {
                let marker = PROJECT_MARKERS.iter().find(|m| dir.join(m).exists())?;
                Some((dir, *marker))
            })
    }

    fn gather(
        &self,
        root: &Path,
        marker: &str,
        repo: Option<Repo>,
        git_failed: bool,
    ) -> ProjectContext {
        let state = repo.as_ref().map(Repo::state);
        let git_failed = git_failed || matches!(state, Some(Err(_)));
        let state = state.and_then(Result::ok);

        // A dev server rewriting `.next` says nothing a source edit wouldn't, and an
        // install can touch thousands of files at once, so artifacts don't count.
        let last_file_change = newest_change(root, |dir| {
            dir.ends_with(".git") || self.recipes.match_dir(dir, self.platform).is_some()
        });
        let last_commit = state.and_then(|state| state.last_commit);
        let last_activity = last_file_change.max(last_commit);

        let mut active_because = Vec::new();
        if self.is_recent(last_activity) {
            active_because.push(BlockReason::RecentActivity);
        }
        if self.processes.uses(root) {
            active_because.push(BlockReason::RunningProcess);
        }
        if state.is_some_and(|s| s.uncommitted_changes) {
            active_because.push(BlockReason::UncommittedChanges);
        }
        if state.is_some_and(|s| s.unpushed_commits) {
            active_because.push(BlockReason::UnpushedCommits);
        }
        if git_failed {
            active_because.push(BlockReason::GitUnavailable);
        }

        let git_status = match (git_failed, state) {
            (true, _) => GitStatus::Unknown,
            (false, Some(state)) => state.git_status(),
            (false, None) => GitStatus::NotARepository,
        };
        ProjectContext {
            root: root.to_owned(),
            marker: marker.to_owned(),
            repo,
            git_status,
            last_activity,
            active_because,
        }
    }

    fn is_recent(&self, time: Option<Timestamp>) -> bool {
        let window = SignedDuration::from_secs(self.active_within_seconds as i64);
        time.is_some_and(|time| self.now.duration_since(time) < window)
    }

    /// True when nothing has happened for at least `seconds`, or nothing is known.
    pub(crate) fn is_idle_for(&self, time: Option<Timestamp>, seconds: u64) -> bool {
        let window = SignedDuration::from_secs(seconds as i64);
        time.is_none_or(|time| self.now.duration_since(time) >= window)
    }

    /// Every reason `path` can't be a candidate. Empty means no check objected,
    /// which is not the same as safe: apply checks all of this again.
    pub fn check(
        &self,
        recipe: &Recipe,
        path: &Path,
        project: Option<&ProjectContext>,
    ) -> Vec<BlockReason> {
        let mut reasons = Vec::new();
        if protected_reason(path, self.home.as_deref()).is_some() {
            reasons.push(BlockReason::ProtectedPath);
        }
        if path.to_str().is_none() {
            reasons.push(BlockReason::NonUnicodePath);
        }
        if recipe
            .protect
            .apps
            .iter()
            .any(|app| self.processes.runs(app))
        {
            reasons.push(BlockReason::AppRunning);
        }
        for protection in &recipe.protect.when {
            self.check_protection(*protection, path, project, &mut reasons);
        }
        let mut unique = Vec::new();
        for reason in reasons {
            if !unique.contains(&reason) {
                unique.push(reason);
            }
        }
        unique
    }

    fn check_protection(
        &self,
        protection: Protection,
        path: &Path,
        project: Option<&ProjectContext>,
        reasons: &mut Vec<BlockReason>,
    ) {
        let repo = project.and_then(|project| project.repo.as_ref());
        let mut ask = |answer: Result<bool, git::GitUnavailable>, reason| match answer {
            Ok(true) => reasons.push(reason),
            Ok(false) => {}
            Err(_) => reasons.push(BlockReason::GitUnavailable),
        };
        match (protection, repo) {
            (Protection::ContainsGitTrackedFiles, Some(repo)) => {
                ask(repo.has_tracked_files(path), BlockReason::GitTrackedFiles);
            }
            (Protection::ContainsUntrackedWork, Some(repo)) => {
                ask(repo.has_untracked_files(path), BlockReason::UntrackedWork);
            }
            (Protection::NotIgnoredByGit, Some(repo)) => {
                ask(
                    repo.is_ignored(path).map(|ignored| !ignored),
                    BlockReason::NotIgnoredByGit,
                );
            }
            // Without git, nothing can vouch for a name like `dist`.
            (Protection::NotIgnoredByGit, None) => reasons.push(BlockReason::NotIgnoredByGit),
            (Protection::InsideActiveProject, _) => {
                reasons.extend(
                    project
                        .iter()
                        .flat_map(|p| p.active_because.iter().copied()),
                );
            }
            (Protection::ContainsGitTrackedFiles | Protection::ContainsUntrackedWork, None) => {}
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("project thread panicked")
}
