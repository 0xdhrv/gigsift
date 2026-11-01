//! Applying plans. A fake recycler stands in for the Trash, so these never touch
//! the real one. The rule under test: if anything differs from the plan, the item
//! stays exactly where it is.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use gigsift_core::{
    ActionStart, ActionStatus, ApplyData, ApplyOutcome, ApplyResult, AuditError, AuditLog,
    BlockReason, ChangeKind, Envelope, PlanData, PlanItem, Profile, RecycleError, Recycler,
};
use gigsift_engine::activity::RunningProcesses;
use gigsift_engine::{Analyzer, ApplyError, apply_plan, create_plan};
use gigsift_recipes::RecipeSet;
use gigsift_scanner::{file_identity, inspect};
use gigsift_store::Store;
use tempfile::{TempDir, tempdir};

const OLD_DATE: &str = "2020-01-01T00:00:00Z";
const KIB_64: usize = 64 * 1024;
const DAY: u64 = 86_400;

type Events = Rc<RefCell<Vec<String>>>;

/// Renames into a temp folder, and notes when it was called.
struct FakeBin {
    dir: PathBuf,
    events: Events,
    refuse: Option<RecycleError>,
}

impl Recycler for FakeBin {
    fn recycle(&self, path: &Path) -> Result<Option<PathBuf>, RecycleError> {
        self.events.borrow_mut().push("recycle".to_owned());
        if let Some(error) = &self.refuse {
            return Err(error.clone());
        }
        let count = fs::read_dir(&self.dir).unwrap().count();
        let target = self.dir.join(count.to_string());
        fs::rename(path, &target).unwrap();
        Ok(Some(target))
    }
}

#[derive(Default)]
struct MemoryLog {
    events: Events,
    fail_start: bool,
    fail_result: bool,
}

impl AuditLog for MemoryLog {
    fn record_start(&mut self, _action: &ActionStart) -> Result<(), AuditError> {
        if self.fail_start {
            return Err("disk full".into());
        }
        self.events.borrow_mut().push("start".to_owned());
        Ok(())
    }

    fn record_result(&mut self, result: &ApplyResult) -> Result<(), AuditError> {
        if self.fail_result {
            return Err("disk full".into());
        }
        self.events
            .borrow_mut()
            .push(format!("{:?}", result.outcome));
        Ok(())
    }
}

struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    bin_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let text = root.to_string_lossy();
        let root = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
        let bin_dir = root.join("bin");
        fs::create_dir(&bin_dir).unwrap();
        let code = root.join("code");
        fs::create_dir(&code).unwrap();
        Self {
            _dir: dir,
            root,
            bin_dir,
        }
    }

    fn code(&self) -> PathBuf {
        self.root.join("code")
    }

    fn bin(&self, events: &Events) -> FakeBin {
        FakeBin {
            dir: self.bin_dir.clone(),
            events: Rc::clone(events),
            refuse: None,
        }
    }

    fn binned(&self) -> usize {
        fs::read_dir(&self.bin_dir).unwrap().count()
    }
}

fn write(root: &Path, relative: &str, bytes: usize) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, vec![b'x'; bytes]).unwrap();
    path
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .env("GIT_AUTHOR_DATE", OLD_DATE)
        .env("GIT_COMMITTER_DATE", OLD_DATE)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

/// A committed project with ignored `node_modules`, the simplest valid candidate.
fn repo(root: &Path, name: &str) -> (PathBuf, PathBuf) {
    let project = root.join(name);
    write(&project, "package.json", 2);
    write(&project, "src/index.js", 10);
    fs::write(project.join(".gitignore"), "node_modules\ndist\n").unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-q", "-m", "init"]);
    write(&project, "node_modules/dep/index.js", KIB_64);
    let modules = project.join("node_modules");
    (project, modules)
}

fn plan(root: &Path, target: u64, window: u64) -> Envelope<PlanData> {
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), window);
    create_plan(root, target, Profile::Developer, &analyzer).unwrap()
}

fn apply_with(
    fixture: &Fixture,
    plan: &Envelope<PlanData>,
    processes: RunningProcesses,
) -> ApplyData {
    let recipes = RecipeSet::builtin();
    let window = plan.data.active_within_seconds;
    let analyzer = Analyzer::new(&recipes, None, processes, window);
    let events = Events::default();
    let mut audit = MemoryLog::default();
    apply_plan(plan, &analyzer, &fixture.bin(&events), &mut audit)
        .unwrap()
        .data
}

fn apply(fixture: &Fixture, plan: &Envelope<PlanData>) -> ApplyData {
    apply_with(fixture, plan, RunningProcesses::none())
}

/// Asserts the only item was skipped and is still in place, then returns why.
fn skipped(fixture: &Fixture, data: &ApplyData, path: &Path) -> ApplyResult {
    assert_eq!(fixture.binned(), 0, "nothing should have moved");
    assert_eq!(data.moved_count, 0);
    assert_eq!(data.moved_bytes, 0);
    assert_eq!(data.skipped_count, 1);
    assert!(
        fs::symlink_metadata(path).is_ok(),
        "{} left",
        path.display()
    );
    let result = data.results[0].clone();
    assert_eq!(result.outcome, ApplyOutcome::Skipped);
    assert!(result.trash_location.is_none());
    result
}

#[test]
fn an_unchanged_item_moves_and_reports_where_it_went() {
    let fixture = Fixture::new();
    let (_, modules) = repo(&fixture.code(), "app");

    let plan = plan(&fixture.code(), 1, 0);
    let data = apply(&fixture, &plan);

    assert!(!modules.exists());
    assert_eq!(fixture.binned(), 1);
    assert_eq!(data.moved_count, 1);
    assert_eq!(data.skipped_count, 0);
    assert_eq!(data.failed_count, 0);
    assert!(data.moved_bytes >= KIB_64 as u64);
    let result = &data.results[0];
    assert_eq!(result.outcome, ApplyOutcome::Moved);
    assert!(result.trash_location.is_some());
    assert!(result.changed.is_empty() && result.blocked_by.is_empty());
}

#[test]
fn every_move_is_logged_before_it_happens_and_finished_after() {
    let fixture = Fixture::new();
    repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);

    let events = Events::default();
    let mut audit = MemoryLog {
        events: Rc::clone(&events),
        ..MemoryLog::default()
    };
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 0);
    apply_plan(&plan, &analyzer, &fixture.bin(&events), &mut audit).unwrap();

    assert_eq!(*events.borrow(), ["start", "recycle", "Moved"]);
}

#[test]
fn nothing_moves_if_the_audit_log_cannot_be_written() {
    let fixture = Fixture::new();
    let (_, modules) = repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);

    let events = Events::default();
    let mut audit = MemoryLog {
        fail_start: true,
        ..MemoryLog::default()
    };
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 0);
    let error = apply_plan(&plan, &analyzer, &fixture.bin(&events), &mut audit).unwrap_err();

    assert!(matches!(error, ApplyError::AuditFailed { moved: 0, .. }));
    assert!(modules.exists());
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());
}

#[test]
fn a_run_stops_at_the_first_result_the_log_cannot_keep() {
    let fixture = Fixture::new();
    repo(&fixture.code(), "one");
    repo(&fixture.code(), "two");
    let plan = plan(&fixture.code(), u64::MAX, 0);
    assert_eq!(plan.data.items.len(), 2);

    let events = Events::default();
    let mut audit = MemoryLog {
        fail_result: true,
        ..MemoryLog::default()
    };
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 0);
    let error = apply_plan(&plan, &analyzer, &fixture.bin(&events), &mut audit).unwrap_err();

    assert!(matches!(error, ApplyError::AuditFailed { moved: 1, .. }));
    assert_eq!(fixture.binned(), 1);
}

#[test]
fn a_refused_trash_leaves_the_item_and_the_run_continues() {
    let fixture = Fixture::new();
    let (_, one) = repo(&fixture.code(), "one");
    let (_, two) = repo(&fixture.code(), "two");
    let plan = plan(&fixture.code(), u64::MAX, 0);

    let events = Events::default();
    let bin = FakeBin {
        refuse: Some(RecycleError::Unavailable("network volume".to_owned())),
        ..fixture.bin(&events)
    };
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 0);
    let mut audit = MemoryLog::default();
    let data = apply_plan(&plan, &analyzer, &bin, &mut audit).unwrap().data;

    assert!(one.exists() && two.exists());
    assert_eq!(data.failed_count, 2);
    assert_eq!(data.moved_bytes, 0);
    assert_eq!(data.results[0].outcome, ApplyOutcome::Failed);
    assert_eq!(data.results[0].moved_bytes, 0);
    assert!(
        data.results[0]
            .error
            .as_deref()
            .unwrap()
            .contains("network volume")
    );
}

#[test]
fn items_outside_the_chosen_list_are_never_touched() {
    let fixture = Fixture::new();
    let (_, one) = repo(&fixture.code(), "one");
    let (_, two) = repo(&fixture.code(), "two");
    let plan = plan(&fixture.code(), 1, 0);
    assert_eq!(plan.data.items.len(), 1);
    assert_eq!(plan.data.not_needed.len(), 1);

    let data = apply(&fixture, &plan);

    assert_eq!(data.moved_count, 1);
    assert_eq!(usize::from(one.exists()) + usize::from(two.exists()), 1);
}

#[test]
fn applying_twice_finds_nothing_left_to_move() {
    let fixture = Fixture::new();
    let (_, modules) = repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);
    apply(&fixture, &plan);
    let data = {
        let fresh = Fixture::new();
        apply(&fresh, &plan)
    };

    assert_eq!(data.moved_count, 0);
    assert_eq!(data.results[0].changed, [ChangeKind::Missing]);
    assert!(!modules.exists());
}

#[test]
fn a_plan_cannot_be_applied_under_a_looser_activity_window() {
    let fixture = Fixture::new();
    repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);

    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 3600);
    let events = Events::default();
    let error = apply_plan(
        &plan,
        &analyzer,
        &fixture.bin(&events),
        &mut MemoryLog::default(),
    )
    .unwrap_err();

    assert_eq!(error, ApplyError::WindowMismatch);
}

#[test]
fn a_new_file_inside_is_a_change() {
    let fixture = Fixture::new();
    let (_, modules) = repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);
    write(&modules, "local-patch.js", KIB_64);

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &modules);
    assert!(result.changed.contains(&ChangeKind::SizeChanged));
}

#[test]
fn a_change_deep_inside_is_caught_even_when_the_top_folder_looks_the_same() {
    let fixture = Fixture::new();
    let (_, modules) = repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);
    write(&modules, "dep/index.js", 2 * KIB_64);

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &modules);
    assert_eq!(result.changed, [ChangeKind::SizeChanged]);
}

#[test]
fn a_folder_replaced_by_another_of_the_same_name_is_a_different_file() {
    let fixture = Fixture::new();
    let (project, modules) = repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);
    fs::rename(&modules, project.join("kept")).unwrap();
    write(&modules, "dep/index.js", KIB_64);

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &modules);
    assert_eq!(result.changed, [ChangeKind::DifferentFile]);
}

#[test]
fn a_missing_item_is_reported_not_treated_as_success() {
    let fixture = Fixture::new();
    let (project, modules) = repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);
    fs::rename(&modules, project.join("kept")).unwrap();

    let data = apply(&fixture, &plan);

    assert_eq!(data.moved_count, 0);
    assert_eq!(data.results[0].changed, [ChangeKind::Missing]);
    assert_eq!(fixture.binned(), 0);
}

#[test]
fn uncommitted_work_since_the_plan_blocks_the_move() {
    let fixture = Fixture::new();
    let (project, modules) = repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);
    write(&project, "src/index.js", 11);

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &modules);
    assert!(result.blocked_by.contains(&BlockReason::UncommittedChanges));
}

#[test]
fn files_git_starts_tracking_after_the_plan_block_the_move() {
    let fixture = Fixture::new();
    let (project, modules) = repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);
    git(&project, &["add", "-f", "node_modules"]);
    git(&project, &["commit", "-q", "-m", "vendor"]);

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &modules);
    assert!(result.blocked_by.contains(&BlockReason::GitTrackedFiles));
}

#[test]
fn a_project_that_became_active_after_the_plan_is_left_alone() {
    let fixture = Fixture::new();
    let (project, modules) = repo(&fixture.code(), "app");
    let long_ago = SystemTime::now() - Duration::from_secs(90 * DAY);
    for name in ["package.json", ".gitignore", "src/index.js"] {
        let file = fs::File::options().write(true).open(project.join(name));
        file.unwrap().set_modified(long_ago).unwrap();
    }
    let plan = plan(&fixture.code(), 1, 30 * DAY);
    assert_eq!(plan.data.items.len(), 1);

    let file = fs::File::options()
        .write(true)
        .open(project.join("src/index.js"))
        .unwrap();
    file.set_modified(SystemTime::now()).unwrap();
    drop(file);

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &modules);
    assert_eq!(result.blocked_by, [BlockReason::RecentActivity]);
}

#[test]
fn an_item_the_recipe_no_longer_recognizes_is_left_alone() {
    let fixture = Fixture::new();
    let (project, modules) = repo(&fixture.code(), "app");
    let plan = plan(&fixture.code(), 1, 0);
    fs::remove_file(project.join("package.json")).unwrap();

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &modules);
    assert_eq!(result.changed, [ChangeKind::NoLongerRecognized]);
}

#[test]
fn a_hand_edited_plan_cannot_redirect_a_move_onto_source() {
    let fixture = Fixture::new();
    let (project, modules) = repo(&fixture.code(), "app");
    let source = project.join("src");
    let mut plan = plan(&fixture.code(), 1, 0);
    let item = &mut plan.data.items[0];
    item.candidate.path = source.to_string_lossy().into_owned();

    let data = apply(&fixture, &plan);

    assert_eq!(fixture.binned(), 0);
    assert!(source.exists() && modules.exists());
    assert_eq!(data.moved_count, 0);
    assert!(!data.results[0].changed.is_empty());
}

#[test]
fn a_hand_edited_plan_cannot_change_which_recipe_applies() {
    let fixture = Fixture::new();
    let (_, modules) = repo(&fixture.code(), "app");
    let mut plan = plan(&fixture.code(), 1, 0);
    plan.data.items[0].recipe_id = "rust-target".to_owned();

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &modules);
    assert_eq!(result.changed, [ChangeKind::NoLongerRecognized]);
}

/// Rewrites a plan item to point at `path`, with an accurate identity and size, so
/// every comparison passes and only the later checks can stop it.
fn forge(item: &mut PlanItem, path: &Path) {
    let size = inspect(path).unwrap();
    item.identity = file_identity(path, size.allocated_bytes).unwrap();
    item.candidate.path = path.to_string_lossy().into_owned();
    item.candidate.logical_bytes = size.logical_bytes;
    item.candidate.allocated_bytes = size.allocated_bytes;
}

#[test]
fn a_forged_plan_with_a_matching_identity_still_cannot_reach_source() {
    let fixture = Fixture::new();
    let (project, _) = repo(&fixture.code(), "app");
    let source = project.join("src");
    let mut plan = plan(&fixture.code(), 1, 0);
    forge(&mut plan.data.items[0], &source);

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &source);
    assert_eq!(result.changed, [ChangeKind::NoLongerRecognized]);
}

#[test]
fn a_forged_plan_with_a_matching_identity_still_cannot_reach_a_protected_path() {
    let fixture = Fixture::new();
    let code = fixture.code();
    repo(&code, "app");
    let mut plan = plan(&code, 1, 0);
    let secret = code.join(".aws/tool");
    write(&secret, "package.json", 2);
    write(&secret, "node_modules/dep/index.js", KIB_64);
    let target = secret.join("node_modules");
    forge(&mut plan.data.items[0], &target);

    let data = apply(&fixture, &plan);

    let result = skipped(&fixture, &data, &target);
    assert!(result.blocked_by.contains(&BlockReason::ProtectedPath));
}

#[test]
fn a_running_app_blocks_moving_its_cache() {
    let fixture = Fixture::new();
    let home = fixture.root.join("home");
    let cache = home.join(".cargo/registry/cache");
    write(&cache, "index/serde.crate", KIB_64);

    let recipes = RecipeSet::builtin();
    let quiet = Analyzer::new(&recipes, Some(home.clone()), RunningProcesses::none(), 0);
    let plan = create_plan(&home, 1, Profile::Developer, &quiet).unwrap();
    assert_eq!(plan.data.items.len(), 1);

    let busy = RunningProcesses::from_parts(Vec::new(), vec!["cargo".to_owned()]);
    let analyzer = Analyzer::new(&recipes, Some(home), busy, 0);
    let events = Events::default();
    let data = apply_plan(
        &plan,
        &analyzer,
        &fixture.bin(&events),
        &mut MemoryLog::default(),
    )
    .unwrap()
    .data;

    assert_eq!(fixture.binned(), 0);
    assert!(cache.exists());
    assert_eq!(data.results[0].blocked_by, [BlockReason::AppRunning]);
}

#[test]
fn the_real_store_keeps_the_full_record_of_a_run() {
    let fixture = Fixture::new();
    let (_, moved) = repo(&fixture.code(), "moved");
    let (kept_project, kept) = repo(&fixture.code(), "kept");
    let plan = plan(&fixture.code(), u64::MAX, 0);
    write(&kept_project, "src/index.js", 11);

    let data_dir = tempdir().unwrap();
    let mut store = Store::open(data_dir.path()).unwrap();
    store.save_plan(&plan).unwrap();
    let saved = store.load_plan(&plan.data.plan_id).unwrap();

    let events = Events::default();
    let recipes = RecipeSet::builtin();
    let analyzer = Analyzer::new(&recipes, None, RunningProcesses::none(), 0);
    let data = apply_plan(&saved, &analyzer, &fixture.bin(&events), &mut store)
        .unwrap()
        .data;

    assert!(!moved.exists());
    assert!(kept.exists());
    let history = store.history(10).unwrap();
    assert_eq!(history.len(), 2);
    let status_of = |name: &str| {
        let entry = history.iter().find(|e| e.path.contains(name)).unwrap();
        entry.status
    };
    assert_eq!(status_of("moved"), ActionStatus::Moved);
    assert_eq!(status_of("kept"), ActionStatus::Skipped);
    let logged: u64 = history.iter().map(|entry| entry.moved_bytes).sum();
    assert_eq!(logged, data.moved_bytes);
    assert!(history.iter().all(|e| e.run_id == data.run_id));
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn a_symlink_swapped_in_after_the_plan_is_never_followed() {
        let fixture = Fixture::new();
        let (project, modules) = repo(&fixture.code(), "app");
        let precious = write(&fixture.root, "precious/thesis.txt", 10);
        let plan = plan(&fixture.code(), 1, 0);
        fs::rename(&modules, project.join("kept")).unwrap();
        symlink(precious.parent().unwrap(), &modules).unwrap();

        let data = apply(&fixture, &plan);

        let result = skipped(&fixture, &data, &modules);
        assert_eq!(result.changed, [ChangeKind::NotADirectory]);
        assert!(precious.exists());
    }

    #[test]
    fn a_parent_swapped_for_a_symlink_is_caught() {
        let fixture = Fixture::new();
        let (project, modules) = repo(&fixture.code(), "app");
        let plan = plan(&fixture.code(), 1, 0);
        let elsewhere = fixture.root.join("elsewhere");
        fs::rename(&project, &elsewhere).unwrap();
        symlink(&elsewhere, &project).unwrap();

        let data = apply(&fixture, &plan);

        let result = skipped(&fixture, &data, &modules);
        assert_eq!(result.changed, [ChangeKind::ParentChanged]);
        assert!(elsewhere.join("node_modules").exists());
    }

    #[test]
    fn a_process_that_started_after_the_plan_blocks_the_move() {
        let fixture = Fixture::new();
        let (project, modules) = repo(&fixture.code(), "app");
        let plan = plan(&fixture.code(), 1, 0);
        let mut child = Command::new("sleep")
            .arg("30")
            .current_dir(project.join("src"))
            .spawn()
            .unwrap();

        let data = apply_with(&fixture, &plan, RunningProcesses::snapshot());

        child.kill().unwrap();
        child.wait().unwrap();
        let result = skipped(&fixture, &data, &modules);
        assert!(result.blocked_by.contains(&BlockReason::RunningProcess));
    }
}
