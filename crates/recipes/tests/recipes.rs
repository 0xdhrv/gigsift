//! Fixture trees for the shipped recipes. Each recipe proves both directions:
//! the real thing matches, and a lookalike does not.

use std::fs;
use std::path::{Path, PathBuf};

use gigsift_core::Action;
use gigsift_recipes::{Platform, RecipeError, RecipeSet};
use tempfile::tempdir;

fn shipped() -> RecipeSet {
    RecipeSet::builtin()
}

fn touch(root: &Path, relative: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "x").unwrap();
    path
}

fn found(root: &Path) -> Vec<(PathBuf, String, String)> {
    shipped()
        .find(root, Platform::Macos, &[])
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
    assert!(set.len() >= 15, "{}", set.len());
    for platform in [Platform::Macos, Platform::Windows] {
        let dir = tempdir().unwrap();
        touch(dir.path(), "app/package.json");
        touch(dir.path(), "app/node_modules/x/index.js");
        let hits = set.find(dir.path(), platform, &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.recipe.action, Action::MoveToTrash);
    }
}

#[test]
fn every_recipe_file_is_built_in() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("recipes");
    assert_eq!(RecipeSet::load_dir(&dir).unwrap().len(), shipped().len());
}

#[test]
fn match_path_finds_the_outermost_recognized_directory() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "app/package.json");
    let inner = touch(
        dir.path(),
        "app/node_modules/dep/node_modules/x/package.json",
    );

    let set = shipped();
    let (matched, hit) = set.match_path(&inner, None, Platform::Macos).unwrap();

    assert_eq!(matched, dir.path().join("app/node_modules"));
    assert_eq!(hit.recipe.id, "node-modules");
}

#[test]
fn match_path_ignores_unrecognized_paths() {
    let dir = tempdir().unwrap();
    let file = touch(dir.path(), "app/src/main.rs");
    assert!(shipped().match_path(&file, None, Platform::Macos).is_none());
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

#[test]
fn nextjs_output_needs_a_package_json() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "web/package.json");
    touch(dir.path(), "web/.next/cache/x");
    touch(dir.path(), "notes/.next/x");

    assert_eq!(
        found(dir.path()),
        vec![hit("web/.next", "next-build", "next build")]
    );
}

#[test]
fn bytecode_folders_need_bytecode_inside() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "tool/__pycache__/a.cpython-312.pyc");
    touch(dir.path(), "other/__pycache__/notes.txt");

    let hits = found(dir.path());

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].1, "python-bytecode");
}

#[test]
fn dist_needs_a_manifest_beside_it() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "web/package.json");
    touch(dir.path(), "web/dist/app.js");
    touch(dir.path(), "docs/dist/guide.md");

    let hits = found(dir.path());

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0, PathBuf::from("web/dist"));
    assert_eq!(hits[0].1, "js-dist");
}

#[test]
fn caches_are_found_by_location_under_home() {
    let home = tempdir().unwrap();
    touch(home.path(), ".cargo/registry/cache/index/serde.crate");
    touch(home.path(), ".m2/repository/x.jar");
    touch(home.path(), "Library/Developer/Xcode/DerivedData/App/x");
    touch(home.path(), "AppData/Local/npm-cache/_cacache/x");

    let set = shipped();
    let ids = |platform| {
        let mut ids: Vec<String> = set
            .home_matches(home.path(), platform)
            .into_iter()
            .map(|(_, hit)| hit.recipe.id.clone())
            .collect();
        ids.sort();
        ids
    };

    assert_eq!(
        ids(Platform::Macos),
        [
            "cargo-registry-cache",
            "maven-repository",
            "xcode-derived-data"
        ]
    );
    assert_eq!(
        ids(Platform::Windows),
        ["cargo-registry-cache", "maven-repository", "npm-cache"]
    );
}

#[test]
fn a_file_inside_a_cache_belongs_to_that_cache() {
    let home = tempdir().unwrap();
    let file = touch(home.path(), ".nuget/packages/x/1.0.0/x.nupkg");

    let set = shipped();
    let (matched, hit) = set
        .match_path(&file, Some(home.path()), Platform::Windows)
        .unwrap();

    assert_eq!(matched, home.path().join(".nuget/packages"));
    assert_eq!(hit.recipe.id, "nuget-cache");
}

#[test]
fn dangerous_tools_are_never_trash_candidates() {
    let set = shipped();
    let home = tempdir().unwrap();
    touch(
        home.path(),
        "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
    );
    touch(home.path(), ".cache/huggingface/hub/models--x/blob");
    for (_, hit) in set.home_matches(home.path(), Platform::Macos) {
        assert_ne!(hit.recipe.action, Action::MoveToTrash, "{}", hit.recipe.id);
    }
}

#[test]
fn packages_inside_a_global_install_are_not_projects() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "node/lib/node_modules/npm/package.json");
    touch(
        dir.path(),
        "node/lib/node_modules/npm/node_modules/semver/index.js",
    );
    touch(dir.path(), "py/lib/site-packages/tool/package.json");
    touch(
        dir.path(),
        "py/lib/site-packages/tool/node_modules/x/index.js",
    );

    assert!(found(dir.path()).is_empty());
}

#[test]
fn walks_skip_git_and_requested_folders() {
    let dir = tempdir().unwrap();
    touch(dir.path(), "skipme/app/package.json");
    touch(dir.path(), "skipme/app/node_modules/x/index.js");
    touch(dir.path(), ".git/app/package.json");
    touch(dir.path(), ".git/app/node_modules/x/index.js");

    let set = shipped();
    let hits = set.find(dir.path(), Platform::Macos, &[dir.path().join("skipme")]);

    assert!(hits.is_empty());
}

#[test]
fn home_paths_cannot_escape_home() {
    assert_invalid(
        &VALID.replace(
            "paths = [\"**/example\"]",
            "paths = []\nhome_paths = [{ path = \"../other\" }]",
        ),
        "inside the home folder",
    );
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
