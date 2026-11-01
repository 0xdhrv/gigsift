//! Planning against real git repositories. The rule under test throughout: when
//! in doubt, a path is blocked, and nothing on disk changes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gigsift_core::{
    BlockReason, Envelope, GitStatus, PlanData, Profile, ProjectsData, SafetyVerdict,
};
use gigsift_engine::activity::RunningProcesses;
use gigsift_engine::{Analyzer, create_plan, explain, find_projects};
use gigsift_recipes::RecipeSet;
use tempfile::{TempDir, tempdir};

const OLD_DATE: &str = "2020-01-01T00:00:00Z";
const KIB_64: usize = 64 * 1024;

fn temp_root() -> (TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let text = root.to_string_lossy();
    let root = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
    (dir, root)
}

fn write(root: &Path, relative: &str, bytes: usize) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, vec![b'x'; bytes]).unwrap();
    path
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .env("GIT_AUTHOR_DATE", OLD_DATE)
        .env("GIT_COMMITTER_DATE", OLD_DATE)
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?}: {status:?}");
}

/// A committed project that ignores `node_modules` and `dist`.
fn repo(root: &Path, name: &str) -> PathBuf {
    let project = root.join(name);
    write(&project, "package.json", 2);
    write(&project, "src/index.js", 10);
    fs::write(project.join(".gitignore"), "node_modules\ndist\n").unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "init"]);
    project
}

fn modules(project: &Path, bytes: usize) -> PathBuf {
    write(project, "node_modules/dep/index.js", bytes);
    project.join("node_modules")
}

fn plan_with(root: &Path, target: u64, analyzer: &Analyzer) -> PlanData {
    create_plan(root, target, Profile::Developer, analyzer)
        .unwrap()
        .data
}

/// Plans with no activity window and no processes, so only git can object.
fn plan(root: &Path, target: u64) -> PlanData {
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 0);
    plan_with(root, target, &analyzer)
}

fn projects(root: &Path, stale: Option<u64>, window: u64) -> ProjectsData {
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), window);
    find_projects(root, stale, &analyzer).unwrap().data
}

fn only_blocked(plan: &PlanData) -> Vec<BlockReason> {
    assert!(plan.items.is_empty(), "{:?}", plan.items);
    assert_eq!(plan.blocked.len(), 1, "{:?}", plan.blocked);
    plan.blocked[0].blocked_by.clone()
}

#[test]
fn a_clean_repo_with_ignored_modules_is_eligible() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    let path = modules(&project, KIB_64);

    let plan = plan(&root, 1);

    assert_eq!(plan.items.len(), 1);
    let candidate = &plan.items[0].candidate;
    assert_eq!(Path::new(&candidate.path), path);
    assert_eq!(candidate.git_status, GitStatus::Clean);
    assert_eq!(candidate.regenerate, "npm install");
    assert!(candidate.requires_confirmation);
    assert!(candidate.reclaimable_bytes_estimate >= KIB_64 as u64);
    assert!(plan.target_met);
    assert!(plan.plan_id.starts_with("plan_"));
    assert!(candidate.candidate_id.starts_with("cand_"));
}

#[test]
fn uncommitted_changes_block_the_project() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    modules(&project, KIB_64);
    write(&project, "src/index.js", 11);

    let reasons = only_blocked(&plan(&root, 1));

    assert!(reasons.contains(&BlockReason::UncommittedChanges));
}

#[test]
fn unpushed_commits_block_the_project() {
    let (_dir, root) = temp_root();
    let remote = root.join("remote.git");
    fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare"]);
    let project = repo(&root, "app");
    modules(&project, KIB_64);
    git(
        &project,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&project, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    assert_eq!(plan(&project, 1).items.len(), 1);

    write(&project, "src/next.js", 10);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "more"]);

    let reasons = only_blocked(&plan(&project, 1));
    assert!(reasons.contains(&BlockReason::UnpushedCommits));
}

#[test]
fn a_repo_with_no_remote_has_nothing_unpushed() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    modules(&project, KIB_64);

    assert_eq!(plan(&root, 1).items.len(), 1);
}

#[test]
fn git_tracked_files_are_source_whatever_the_folder_is_called() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    modules(&project, KIB_64);
    git(&project, &["add", "-f", "node_modules"]);
    git(&project, &["commit", "-q", "-m", "vendor"]);

    let reasons = only_blocked(&plan(&root, 1));

    assert!(reasons.contains(&BlockReason::GitTrackedFiles));
}

#[test]
fn untracked_files_inside_dist_are_never_assumed_disposable() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    fs::write(project.join(".gitignore"), "node_modules\n").unwrap();
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "stop ignoring dist"]);
    write(&project, "dist/handwritten.js", KIB_64);

    let reasons = only_blocked(&plan(&root, 1));

    assert!(reasons.contains(&BlockReason::UntrackedWork));
    assert!(reasons.contains(&BlockReason::NotIgnoredByGit));
}

#[test]
fn an_ignored_dist_is_eligible() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    write(&project, "dist/bundle.js", KIB_64);

    assert_eq!(plan(&root, 1).items.len(), 1);
}

#[test]
fn dist_without_git_has_nothing_vouching_for_it() {
    let (_dir, root) = temp_root();
    write(&root, "app/package.json", 2);
    write(&root, "app/dist/bundle.js", KIB_64);

    let reasons = only_blocked(&plan(&root, 1));

    assert_eq!(reasons, [BlockReason::NotIgnoredByGit]);
}

#[test]
fn a_project_outside_git_stays_eligible_with_less_confidence() {
    let (_dir, root) = temp_root();
    let tracked = repo(&root, "tracked");
    modules(&tracked, KIB_64);
    write(&root, "loose/package.json", 2);
    modules(&root.join("loose"), KIB_64);

    let plan = plan(&root, u64::MAX);

    let confidence_of = |name: &str| {
        let item = plan.items.iter().find(|i| i.candidate.path.contains(name));
        item.unwrap().candidate.confidence
    };
    assert!(confidence_of("loose") < confidence_of("tracked"));
}

#[test]
fn recent_changes_mark_a_project_active() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    modules(&project, KIB_64);

    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 86_400);
    let reasons = only_blocked(&plan_with(&root, 1, &analyzer));

    assert!(reasons.contains(&BlockReason::RecentActivity));
}

/// Backdates every file outside `.git`.
fn age_files(dir: &Path, days_ago: u64) {
    let time = SystemTime::now() - Duration::from_secs(days_ago * 86_400);
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        if path.is_dir() {
            age_files(&path, days_ago);
        } else {
            let file = fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(time).unwrap();
        }
    }
}

#[test]
fn artifact_churn_does_not_keep_a_project_active() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    age_files(&project, 90);
    modules(&project, KIB_64);

    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 30 * 86_400);

    assert_eq!(plan_with(&root, 1, &analyzer).items.len(), 1);
}

#[test]
fn the_oldest_lowest_risk_items_are_picked_until_the_target_is_met() {
    let (_dir, root) = temp_root();
    let newest = repo(&root, "newest");
    let oldest = repo(&root, "oldest");
    let middle = repo(&root, "middle");
    modules(&newest, KIB_64);
    modules(&oldest, KIB_64);
    modules(&middle, KIB_64);
    age_files(&newest, 10);
    age_files(&middle, 20);
    age_files(&oldest, 30);

    let plan = plan(&root, KIB_64 as u64 * 2 - 1);

    let picked: Vec<&str> = plan
        .items
        .iter()
        .map(|item| Path::new(&item.candidate.path))
        .map(|path| {
            path.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
        })
        .collect();
    assert_eq!(picked, ["oldest", "middle"]);
    assert_eq!(plan.not_needed.len(), 1);
    assert!(plan.target_met);
    assert_eq!(plan.shortfall_bytes, 0);
}

#[test]
fn larger_items_are_picked_first_so_the_plan_stays_short() {
    let (_dir, root) = temp_root();
    let small = repo(&root, "small");
    modules(&small, KIB_64);
    let large = repo(&root, "large");
    modules(&large, 16 * KIB_64);

    let plan = plan(&root, KIB_64 as u64);

    assert_eq!(plan.items.len(), 1);
    assert!(plan.items[0].candidate.path.contains("large"));
}

#[test]
fn items_git_can_vouch_for_come_before_larger_ones_it_cannot() {
    let (_dir, root) = temp_root();
    let verified = repo(&root, "verified");
    modules(&verified, KIB_64);
    write(&root, "loose/package.json", 2);
    modules(&root.join("loose"), 16 * KIB_64);

    let plan = plan(&root, 1);

    assert_eq!(plan.items.len(), 1);
    assert!(plan.items[0].candidate.path.contains("verified"));
}

#[test]
fn a_shortfall_is_reported_rather_than_hidden() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    modules(&project, KIB_64);

    let plan = plan(&root, 1 << 40);

    assert!(!plan.target_met);
    assert_eq!(
        plan.shortfall_bytes,
        (1 << 40) - plan.reclaimable_bytes_estimate
    );
}

#[test]
fn storage_shared_with_something_outside_is_not_counted() {
    let (_dir, root) = temp_root();
    let store = write(&root, "store/shared.bin", KIB_64);
    let project = repo(&root, "app");
    fs::create_dir_all(project.join("node_modules/dep")).unwrap();
    fs::hard_link(&store, project.join("node_modules/dep/index.js")).unwrap();

    let plan = plan(&root, 1);

    assert!(plan.items.is_empty());
    assert_eq!(plan.not_needed.len(), 1);
    assert_eq!(plan.not_needed[0].candidate.reclaimable_bytes_estimate, 0);
    assert!(!plan.target_met);
}

#[test]
fn protected_locations_stay_blocked_even_when_a_recipe_matches() {
    let (_dir, root) = temp_root();
    write(&root, ".aws/tool/package.json", 2);
    modules(&root.join(".aws/tool"), KIB_64);

    let reasons = only_blocked(&plan(&root, 1));

    assert!(reasons.contains(&BlockReason::ProtectedPath));
}

fn fake_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    write(&home, ".cargo/registry/cache/index/serde.crate", KIB_64);
    write(&home, ".m2/repository/lib.jar", KIB_64);
    write(&home, "code/app/package.json", 2);
    modules(&home.join("code/app"), KIB_64);
    // A tool installed in a dot-folder, shaped exactly like a project.
    write(&home, ".local/share/tool/package.json", 2);
    modules(&home.join(".local/share/tool"), KIB_64);
    home
}

#[test]
fn caches_join_the_plan_but_after_projects_and_report_only_data_stays_out() {
    let (_dir, root) = temp_root();
    let home = fake_home(&root);

    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, Some(home.clone()), RunningProcesses::none(), 0);
    let plan = plan_with(&home, u64::MAX, &analyzer);

    let paths: Vec<&str> = plan
        .items
        .iter()
        .map(|i| i.candidate.path.as_str())
        .collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
    assert!(paths[0].contains("node_modules"));
    assert!(paths[1].contains("registry"));
    assert!(plan.items[1].candidate.project.is_none());
    assert_eq!(plan.advisories.len(), 1);
    assert!(plan.advisories[0].path.contains("repository"));
}

#[test]
fn caches_outside_the_requested_folder_are_left_out() {
    let (_dir, root) = temp_root();
    let home = fake_home(&root);

    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, Some(home.clone()), RunningProcesses::none(), 0);
    let plan = plan_with(&home.join("code"), u64::MAX, &analyzer);

    assert_eq!(plan.items.len(), 1);
    assert!(plan.advisories.is_empty());
}

#[test]
fn every_item_carries_the_identity_apply_will_need() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    modules(&project, KIB_64);

    let plan = plan(&root, 1);

    let identity = plan.items[0].identity;
    assert_ne!(identity.file, 0);
    assert!(identity.modified_at.is_some());
    assert_eq!(
        identity.allocated_bytes,
        plan.items[0].candidate.allocated_bytes
    );
}

#[test]
fn a_monorepo_is_one_project() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "mono");
    write(&project, "packages/a/package.json", 2);
    write(&project, "packages/b/package.json", 2);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "packages"]);
    modules(&project.join("packages/a"), KIB_64);
    modules(&project.join("packages/b"), KIB_64);

    let data = projects(&root, None, 0);

    assert_eq!(data.projects.len(), 1);
    assert_eq!(data.projects[0].artifacts.len(), 2);
    assert_eq!(data.projects[0].marker, ".git");
}

#[test]
fn the_stale_filter_hides_recent_projects() {
    let (_dir, root) = temp_root();
    let old = repo(&root, "old");
    modules(&old, KIB_64);
    age_files(&old, 90);
    let fresh = repo(&root, "fresh");
    modules(&fresh, KIB_64);

    let data = projects(&root, Some(30 * 86_400), 0);

    assert_eq!(data.projects.len(), 1);
    assert_eq!(data.hidden_as_recent, 1);
    assert!(data.projects[0].root.ends_with("old"));
}

#[test]
fn projects_are_ordered_by_what_can_be_recovered() {
    let (_dir, root) = temp_root();
    let small = repo(&root, "small");
    modules(&small, KIB_64);
    let large = repo(&root, "large");
    modules(&large, 8 * KIB_64);

    let data = projects(&root, None, 0);

    assert!(data.projects[0].root.ends_with("large"));
    assert!(
        data.projects[0].reclaimable_bytes_estimate > data.projects[1].reclaimable_bytes_estimate
    );
}

#[test]
fn blocked_artifacts_do_not_count_as_recoverable() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    modules(&project, KIB_64);
    write(&project, "src/index.js", 11);

    let data = projects(&root, None, 0);

    assert_eq!(data.projects[0].reclaimable_bytes_estimate, 0);
    assert!(data.projects[0].active);
    assert_eq!(data.projects[0].git_status, GitStatus::Dirty);
}

#[test]
fn explain_reports_the_same_blocks_as_a_plan() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    let path = modules(&project, KIB_64);
    write(&project, "src/index.js", 11);

    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 0);
    let safety = explain(&path, &analyzer).unwrap().data.safety;

    assert_eq!(safety.verdict, SafetyVerdict::Blocked);
    assert!(safety.blocked_by.contains(&BlockReason::UncommittedChanges));
}

#[test]
fn a_home_folder_kept_in_git_does_not_swallow_its_projects() {
    let (_dir, root) = temp_root();
    let home = repo(&root, "home");
    fs::write(home.join(".gitignore"), "*\n").unwrap();
    write(&home, "code/app/package.json", 2);
    modules(&home.join("code/app"), KIB_64);

    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, Some(home.clone()), RunningProcesses::none(), 0);
    let data = find_projects(&home.join("code"), None, &analyzer)
        .unwrap()
        .data;

    assert_eq!(data.projects.len(), 1);
    assert!(data.projects[0].root.ends_with("app"));
    assert_eq!(data.projects[0].git_status, GitStatus::NotARepository);
}

/// Path, size, and modification time of everything under `root`, including `.git`.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut seen = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        seen.insert(path.clone(), (meta.len(), meta.modified().unwrap()));
        if meta.is_dir() {
            pending.extend(fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        }
    }
    seen
}

fn run_every_read_only_operation(root: &Path, path: &Path) -> Envelope<PlanData> {
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::snapshot(), 86_400);
    explain(path, &analyzer).unwrap();
    find_projects(root, None, &analyzer).unwrap();
    create_plan(root, 1, Profile::Developer, &analyzer).unwrap()
}

#[test]
fn planning_changes_nothing_on_disk() {
    let (_dir, root) = temp_root();
    let clean = repo(&root, "clean");
    let clean_modules = modules(&clean, KIB_64);
    let dirty = repo(&root, "dirty");
    modules(&dirty, KIB_64);
    write(&dirty, "src/index.js", 11);
    write(&dirty, "untracked.txt", 5);
    write(&root, "loose/package.json", 2);
    write(&root, "loose/dist/bundle.js", KIB_64);
    let before = snapshot(&root);

    run_every_read_only_operation(&root, &clean_modules);

    assert_eq!(snapshot(&root), before);
}

#[test]
fn planning_does_not_refresh_the_git_index() {
    let (_dir, root) = temp_root();
    let project = repo(&root, "app");
    let path = modules(&project, KIB_64);
    // Same bytes, new timestamp: the case a plain `git status` writes the index for.
    let file = fs::File::options()
        .write(true)
        .open(project.join("src/index.js"))
        .unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        .unwrap();
    drop(file);
    let index = project.join(".git/index");
    let before = fs::read(&index).unwrap();

    run_every_read_only_operation(&root, &path);

    assert_eq!(fs::read(&index).unwrap(), before);

    // Proves the fixture bites: without the guard, git does rewrite the file.
    git(&project, &["status", "--porcelain"]);
    assert_ne!(fs::read(&index).unwrap(), before);
}

#[cfg(unix)]
mod processes {
    use super::*;

    #[test]
    fn a_process_working_in_a_project_marks_it_active() {
        let (_dir, root) = temp_root();
        let project = repo(&root, "app");
        modules(&project, KIB_64);
        let mut child = Command::new("sleep")
            .arg("30")
            .current_dir(project.join("src"))
            .spawn()
            .unwrap();

        let recipes = RecipeSet::builtin();
        let analyzer = Analyzer::new(&recipes, None, RunningProcesses::snapshot(), 0);
        let plan = plan_with(&root, 1, &analyzer);

        child.kill().unwrap();
        child.wait().unwrap();
        assert!(only_blocked(&plan).contains(&BlockReason::RunningProcess));
    }

    #[test]
    fn running_apps_are_found_by_name_ignoring_case() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let processes = RunningProcesses::snapshot();
        child.kill().unwrap();
        child.wait().unwrap();

        assert!(processes.runs("SLEEP"));
        assert!(!processes.runs("no-such-app"));
    }
}
