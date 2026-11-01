use std::fs;
use std::path::{Path, PathBuf};

use gigsift_core::{
    Action, CompletenessStatus, ExplainData, PathKind, ProtectedReason, Protection, SafetyVerdict,
};
use gigsift_engine::explain;
use gigsift_recipes::RecipeSet;
use tempfile::{TempDir, tempdir};

/// macOS temp paths run through a symlink, and explain reports resolved paths.
fn temp_root() -> (TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

fn write(root: &Path, relative: &str, bytes: usize) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, vec![7u8; bytes]).unwrap();
    path
}

fn run(path: &Path) -> ExplainData {
    explain(path, &RecipeSet::builtin()).unwrap().data
}

#[test]
fn a_recognized_directory_is_explained_but_not_called_safe() {
    let (_dir, root) = temp_root();
    write(&root, "app/package.json", 10);
    write(&root, "app/pnpm-lock.yaml", 10);
    write(&root, "app/node_modules/left-pad/index.js", 4096);

    let data = run(&root.join("app/node_modules"));

    assert_eq!(data.kind, PathKind::Directory);
    assert_eq!(data.logical_bytes, 4096);
    let recognized = data.recognized.unwrap();
    assert_eq!(recognized.recipe_id, "node-modules");
    assert_eq!(recognized.regenerate, "pnpm install");
    assert_eq!(recognized.recommended_action, Action::MoveToTrash);
    assert_eq!(data.safety.verdict, SafetyVerdict::ReviewRequired);
    assert!(
        data.safety
            .checks_pending
            .contains(&Protection::ContainsUntrackedWork)
    );
}

#[test]
fn a_file_inside_a_recognized_directory_belongs_to_the_outer_project() {
    let (_dir, root) = temp_root();
    write(&root, "app/package.json", 10);
    let inner = write(&root, "app/node_modules/dep/package.json", 10);

    let data = run(&inner);

    let recognized = data.recognized.unwrap();
    assert_eq!(
        recognized.matched_path,
        root.join("app/node_modules").to_string_lossy()
    );
    let project = data.project.unwrap();
    assert_eq!(project.root, root.join("app").to_string_lossy());
    assert_eq!(project.marker, "package.json");
}

#[test]
fn an_unrecognized_file_is_unknown_not_disposable() {
    let (_dir, root) = temp_root();
    write(&root, "app/.git/HEAD", 10);
    let file = write(&root, "app/src/main.rs", 100);

    let data = run(&file);

    assert_eq!(data.kind, PathKind::File);
    assert_eq!(data.logical_bytes, 100);
    assert!(data.recognized.is_none());
    assert_eq!(data.safety.verdict, SafetyVerdict::Unknown);
    assert!(data.safety.checks_pending.is_empty());
    assert_eq!(data.project.unwrap().marker, ".git");
}

#[test]
fn protection_beats_a_recipe_match() {
    let (_dir, root) = temp_root();
    write(&root, "app/package.json", 10);
    let secret = write(&root, "app/node_modules/dep/.env", 10);

    let data = run(&secret);

    assert!(data.recognized.is_some());
    assert_eq!(data.safety.verdict, SafetyVerdict::Protected);
    assert_eq!(
        data.safety.protected_because,
        Some(ProtectedReason::Credentials)
    );
    assert!(data.safety.checks_pending.is_empty());
}

#[test]
fn git_metadata_is_protected() {
    let (_dir, root) = temp_root();
    write(&root, "app/.git/HEAD", 10);

    let data = run(&root.join("app/.git"));

    assert_eq!(
        data.safety.protected_because,
        Some(ProtectedReason::GitMetadata)
    );
}

#[test]
fn hard_linked_files_report_their_link_count() {
    let (_dir, root) = temp_root();
    let file = write(&root, "a.bin", 100);
    fs::hard_link(&file, root.join("b.bin")).unwrap();

    assert_eq!(run(&file).hard_links, 2);
}

#[test]
fn a_missing_path_is_an_error() {
    let (_dir, root) = temp_root();
    assert!(explain(&root.join("nope"), &RecipeSet::builtin()).is_err());
}

#[test]
fn a_directory_scan_carries_its_completeness() {
    let (_dir, root) = temp_root();
    write(&root, "data/a.bin", 10);
    let report = explain(&root.join("data"), &RecipeSet::builtin()).unwrap();
    assert_eq!(
        report.scan_completeness.status,
        CompletenessStatus::Complete
    );
    assert_eq!(report.scan_completeness.directories_visited, 1);
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn symlinks_are_described_not_followed() {
        let (_dir, root) = temp_root();
        write(&root, "real/big.bin", 8192);
        symlink(root.join("real"), root.join("link")).unwrap();

        let data = run(&root.join("link"));

        assert_eq!(data.kind, PathKind::Symlink);
        assert_eq!(
            data.symlink_target.as_deref(),
            Some(&*root.join("real").to_string_lossy())
        );
        assert_eq!(data.logical_bytes, 0);
    }

    #[test]
    fn a_symlink_into_a_secret_folder_cannot_hide_a_child_path() {
        let (_dir, root) = temp_root();
        write(&root, ".ssh/id_custom", 10);
        symlink(root.join(".ssh"), root.join("innocent")).unwrap();

        let data = run(&root.join("innocent/id_custom"));

        assert_eq!(
            data.safety.protected_because,
            Some(ProtectedReason::Credentials)
        );
    }

    #[test]
    fn dot_dot_cannot_hide_a_protected_location() {
        let (_dir, root) = temp_root();
        write(&root, ".ssh/config", 10);
        write(&root, "app/file.txt", 10);

        let data = run(&root.join("app/../.ssh/config"));

        assert_eq!(
            data.safety.protected_because,
            Some(ProtectedReason::Credentials)
        );
    }
}
