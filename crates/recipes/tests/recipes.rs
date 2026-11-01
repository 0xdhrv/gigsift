//! Fixture trees for the shipped recipes. Each recipe proves both directions:
//! the real thing matches, and a lookalike does not.

use std::fs;
use std::path::{Path, PathBuf};

use gigsift_core::Action;
use gigsift_recipes::{Platform, RecipeError, RecipeSet};
use tempfile::tempdir;

fn shipped() -> RecipeSet {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../recipes");
    RecipeSet::load_dir(&dir).unwrap()
}

fn touch(root: &Path, relative: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "x").unwrap();
    path
}

fn found(root: &Path) -> Vec<(PathBuf, String, String)> {
    shipped()
        .find(root, Platform::Macos)
        .into_iter()
        .map(|(path, hit)| {
            let relative = path.strip_prefix(root).unwrap().to_owned();
            (relative, hit.recipe.id.clone(), hit.regenerate.to_owned())
        })
        .collect()
}

fn hit(path: &str, id: &str, regenerate: &str) -> (PathBuf, String, String) {
    (path.into(), id.to_owned(), regenerate.to_owned())
}

#[test]
fn shipped_recipes_load_and_never_recommend_hard_delete() {
    let set = shipped();
    assert_eq!(set.len(), 3);
    for platform in [Platform::Macos, Platform::Windows] {
        let dir = tempdir().unwrap();
        touch(dir.path(), "app/package.json");
        touch(dir.path(), "app/node_modules/x/index.js");
        let hits = set.find(dir.path(), platform);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.recipe.action, Action::MoveToTrash);
    }
}

#[test]
fn node_modules_matches_beside_package_json() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "app/package.json");
    touch(dir.path(), "app/node_modules/left-pad/index.js");

    assert_eq!(
        found(dir.path()),
        vec![hit("app/node_modules", "node-modules", "npm install")]
    );
}

#[test]
fn node_modules_hint_follows_the_lockfile() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "app/package.json");
    touch(dir.path(), "app/pnpm-lock.yaml");
    touch(dir.path(), "app/node_modules/left-pad/index.js");

    assert_eq!(
        found(dir.path()),
        vec![hit("app/node_modules", "node-modules", "pnpm install")]
    );
}

#[test]
fn node_modules_without_a_manifest_is_left_alone() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "stray/node_modules/left-pad/index.js");

    assert!(found(dir.path()).is_empty());
}

#[test]
fn nested_node_modules_belong_to_the_outer_match() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "app/package.json");
    touch(dir.path(), "app/node_modules/dep/package.json");
    touch(
        dir.path(),
        "app/node_modules/dep/node_modules/inner/index.js",
    );

    assert_eq!(found(dir.path()).len(), 1);
}

#[test]
fn rust_target_needs_cargo_toml_and_cargo_output() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "crate/Cargo.toml");
    touch(dir.path(), "crate/target/CACHEDIR.TAG");
    touch(dir.path(), "crate/target/debug/app");

    assert_eq!(
        found(dir.path()),
        vec![hit("crate/target", "rust-target", "cargo build")]
    );
}

#[test]
fn a_hand_made_target_folder_is_left_alone() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "crate/Cargo.toml");
    touch(dir.path(), "crate/target/my-notes.md");
    touch(dir.path(), "web/target/CACHEDIR.TAG");

    assert!(found(dir.path()).is_empty());
}

#[test]
fn python_venv_matches_on_pyvenv_cfg() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "tool/uv.lock");
    touch(dir.path(), "tool/.venv/pyvenv.cfg");
    touch(dir.path(), "tool/.venv/bin/python");
    touch(dir.path(), "other/venv/pyvenv.cfg");

    assert_eq!(
        found(dir.path()),
        vec![
            hit(
                "other/venv",
                "python-venv",
                "python -m venv .venv, then reinstall the packages you need"
            ),
            hit("tool/.venv", "python-venv", "uv sync"),
        ]
    );
}

#[test]
fn a_source_folder_named_venv_is_left_alone() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "tool/venv/helpers.py");

    assert!(found(dir.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn symlinks_named_like_candidates_are_not_matches() {
    let dir = tempdir().unwrap();
    let elsewhere = tempdir().unwrap();
    touch(elsewhere.path(), "left-pad/index.js");
    touch(dir.path(), "app/package.json");
    std::os::unix::fs::symlink(elsewhere.path(), dir.path().join("app/node_modules")).unwrap();

    assert!(found(dir.path()).is_empty());
}

const VALID: &str = r#"
format_version = 1
id = "example"
name = "Example"
platforms = ["macos"]
category = "build_artifact"
action = "report_only"
risk = "low"
rebuild_cost = { time = "seconds", network = "none" }
[detect]
paths = ["**/example"]
inner_markers = ["MARKER"]
[protect]
when = ["inside_active_project"]
[explain]
reason = "r"
loses = "l"
[[regenerate]]
hint = "h"
"#;

fn add(text: &str) -> Result<(), RecipeError> {
    RecipeSet::default().add(text, "test")
}

#[test]
fn a_valid_recipe_loads() {
    add(VALID).unwrap();
}

#[test]
fn recipes_cannot_carry_a_command() {
    let shell_field = VALID.replace("risk = \"low\"", "risk = \"low\"\ncommand = \"rm -rf ~\"");
    assert!(matches!(add(&shell_field), Err(RecipeError::Parse { .. })));

    let shell_regenerate = VALID.replace("hint = \"h\"", "hint = \"h\"\nrun = \"npm install\"");
    assert!(matches!(
        add(&shell_regenerate),
        Err(RecipeError::Parse { .. })
    ));
}

#[test]
fn recipes_cannot_invent_an_action() {
    let delete = VALID.replace("report_only", "permanently_delete");
    assert!(matches!(add(&delete), Err(RecipeError::Parse { .. })));
}

fn assert_invalid(text: &str, expected: &str) {
    match add(text) {
        Err(RecipeError::Invalid { problem, .. }) => {
            assert!(problem.contains(expected), "{problem}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn recipes_must_prove_more_than_a_name() {
    assert_invalid(
        &VALID.replace("inner_markers = [\"MARKER\"]", ""),
        "a name alone proves nothing",
    );
}

#[test]
fn markers_cannot_reach_outside_the_directory() {
    assert_invalid(
        &VALID.replace("\"MARKER\"", "\"../../.ssh/id_rsa\""),
        "plain file name",
    );
}

#[test]
fn regenerate_must_end_in_a_fallback() {
    assert_invalid(
        &VALID.replace("hint = \"h\"", "marker = \"a.lock\"\nhint = \"h\""),
        "regenerate must end",
    );
}

#[test]
fn future_formats_are_refused() {
    assert_invalid(
        &VALID.replace("format_version = 1", "format_version = 2"),
        "not supported",
    );
}

#[test]
fn duplicate_ids_are_refused() {
    let mut set = RecipeSet::default();
    set.add(VALID, "first").unwrap();
    assert!(matches!(
        set.add(VALID, "second"),
        Err(RecipeError::Invalid { .. })
    ));
}
