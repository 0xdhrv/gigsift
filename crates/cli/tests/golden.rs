//! Golden snapshots of `--json` output. They guard the contract: a changed
//! snapshot is a changed public API. Regenerate with `UPDATE_GOLDEN=1`.
//!
//! The tree is deterministic, but a few values are not: temp paths, timestamps,
//! and allocated sizes, which depend on the filesystem. Those are normalized.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

/// Reads the published schema from disk, so this tests what consumers get.
fn validator(name: &str) -> jsonschema::Validator {
    let path = repo_root().join(format!("schema/v1/{name}.schema.json"));
    let schema: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn gigsift(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gigsift"))
        .args(args)
        .output()
        .unwrap()
}

fn fixture() -> TempDir {
    let dir = tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("app/src")).unwrap();
    fs::create_dir_all(root.join("app/node_modules/left-pad")).unwrap();
    fs::create_dir_all(root.join("media")).unwrap();
    fs::write(root.join("app/src/main.ts"), vec![b'x'; 10_000]).unwrap();
    fs::write(
        root.join("app/node_modules/left-pad/index.js"),
        vec![b'x'; 200_000],
    )
    .unwrap();
    fs::write(root.join("media/clip.mov"), vec![b'x'; 2_000_000]).unwrap();
    fs::hard_link(root.join("media/clip.mov"), root.join("app/clip-link.mov")).unwrap();
    fs::write(root.join("app/package.json"), "{}").unwrap();
    fs::write(root.join("app/.env"), "SECRET=1").unwrap();
    dir
}

/// Replaces machine-specific values, leaving the shape and every logical size.
fn normalize(value: &mut Value, root: &str) {
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                match key.as_str() {
                    "scanned_at" | "last_activity" | "modified_at" => {
                        *item = json!("<timestamp>");
                    }
                    "allocated_bytes"
                    | "exclusive_bytes"
                    | "reclaimable_bytes_estimate"
                    | "volume"
                    | "file" => *item = json!("<machine>"),
                    "plan_id" => *item = json!("<plan_id>"),
                    "candidate_id" => *item = json!("<candidate_id>"),
                    _ => normalize(item, root),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| normalize(item, root)),
        Value::String(text) => {
            *text = text.replace(root, "<root>").replace('\\', "/");
        }
        _ => {}
    }
}

fn check_golden(name: &str, output: &Output, root: &Path) {
    assert!(output.status.success(), "{output:?}");
    let mut value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema = name.split('-').next().unwrap();
    assert!(
        validator(schema).is_valid(&value),
        "{name} output violates its published schema"
    );

    normalize(&mut value, &root.to_string_lossy());
    let path = repo_root().join(format!("crates/cli/tests/golden/{name}.json"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        let text = serde_json::to_string_pretty(&value).unwrap() + "\n";
        fs::write(&path, text).unwrap();
    }
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let expected: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(expected, value, "{name} drifted from its golden file");
}

#[test]
fn scan_json_matches_golden() {
    let dir = fixture();
    let root = dir.path().to_str().unwrap();
    check_golden("scan", &gigsift(&["scan", root, "--json"]), dir.path());
}

#[test]
fn top_json_matches_golden() {
    let dir = fixture();
    let root = dir.path().to_str().unwrap();
    let output = gigsift(&["top", root, "--min-size", "100kb", "--limit", "3", "--json"]);
    check_golden("top", &output, dir.path());
}

/// Explain reports resolved paths, so the fixture root must be resolved too.
fn resolved_root(dir: &TempDir) -> PathBuf {
    let root = fs::canonicalize(dir.path()).unwrap();
    let text = root.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

fn explain_golden(name: &str, relative: &str) {
    let dir = fixture();
    let root = resolved_root(&dir);
    let target = root.join(relative);
    let output = gigsift(&[
        "explain",
        target.to_str().unwrap(),
        "--active-within",
        "0d",
        "--json",
    ]);
    check_golden(name, &output, &root);
}

#[test]
fn explain_json_for_a_recognized_directory_matches_golden() {
    explain_golden("explain-recognized", "app/node_modules");
}

#[test]
fn explain_json_for_an_unknown_file_matches_golden() {
    explain_golden("explain-unknown", "app/src/main.ts");
}

#[test]
fn explain_json_for_a_protected_file_matches_golden() {
    explain_golden("explain-protected", "app/.env");
}

#[test]
fn explain_text_never_calls_anything_safe_to_remove() {
    let dir = fixture();
    let root = resolved_root(&dir);
    let target = root.join("app/node_modules");

    let output = gigsift(&["explain", target.to_str().unwrap(), "--active-within", "0d"]);

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Review required"), "{text}");
    assert!(text.contains("npm install"), "{text}");
}

#[test]
fn explain_blocks_a_project_that_just_changed() {
    let dir = fixture();
    let root = resolved_root(&dir);
    let target = root.join("app/node_modules");

    let output = gigsift(&["explain", target.to_str().unwrap()]);

    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Blocked"), "{text}");
    assert!(text.contains("project changed recently"), "{text}");
}

fn gigsift_with_data(data_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gigsift"))
        .args(args)
        .env("GIGSIFT_DATA_DIR", data_dir)
        .output()
        .unwrap()
}

#[test]
fn projects_json_matches_golden() {
    let dir = fixture();
    let root = resolved_root(&dir);
    let output = gigsift(&[
        "projects",
        root.to_str().unwrap(),
        "--active-within",
        "0d",
        "--json",
    ]);
    check_golden("projects", &output, &root);
}

#[test]
fn clean_plan_json_matches_golden_and_show_returns_the_same_plan() {
    let dir = fixture();
    let root = resolved_root(&dir);
    let data = tempdir().unwrap();

    let output = gigsift_with_data(
        data.path(),
        &[
            "clean",
            "plan",
            root.to_str().unwrap(),
            "--target",
            "1kb",
            "--active-within",
            "0d",
            "--json",
        ],
    );
    check_golden("plan", &output, &root);

    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = plan["data"]["plan_id"].as_str().unwrap();
    let shown = gigsift_with_data(data.path(), &["clean", "show", id, "--json"]);
    assert!(shown.status.success(), "{shown:?}");
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown, plan);
}

#[test]
fn clean_show_reports_a_missing_plan_in_the_error_contract() {
    let data = tempdir().unwrap();
    let id = "plan_01JABCDEFGHJKMNPQRSTVWXYZ0";

    let output = gigsift_with_data(data.path(), &["clean", "show", id, "--json"]);

    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(validator("error").is_valid(&value));
    assert_eq!(value["error"]["code"], "plan_not_found");
}

fn saved_plan(root: &Path, data: &Path) -> String {
    let output = gigsift_with_data(
        data,
        &[
            "clean",
            "plan",
            root.to_str().unwrap(),
            "--target",
            "1kb",
            "--active-within",
            "0d",
            "--json",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    plan["data"]["plan_id"].as_str().unwrap().to_owned()
}

#[test]
fn apply_refuses_without_confirmation_when_no_one_can_answer() {
    let dir = fixture();
    let root = resolved_root(&dir);
    let data = tempdir().unwrap();
    let id = saved_plan(&root, data.path());

    for args in [
        vec!["clean", "apply", id.as_str()],
        vec!["clean", "apply", id.as_str(), "--json"],
    ] {
        let output = gigsift_with_data(data.path(), &args);
        assert!(!output.status.success(), "{output:?}");
        assert!(root.join("app/node_modules").exists());
    }

    let output = gigsift_with_data(data.path(), &["clean", "apply", &id, "--json"]);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(validator("error").is_valid(&value));
    assert_eq!(value["error"]["code"], "confirmation_required");

    let history = gigsift_with_data(data.path(), &["history", "--json"]);
    let value: Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(value["data"]["actions"], json!([]));
}

#[test]
fn apply_with_an_unknown_plan_moves_nothing() {
    let data = tempdir().unwrap();
    let id = "plan_01JABCDEFGHJKMNPQRSTVWXYZ0";

    let output = gigsift_with_data(data.path(), &["clean", "apply", id, "--yes", "--json"]);

    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "plan_not_found");
}

#[test]
fn history_json_matches_its_schema_when_empty() {
    let data = tempdir().unwrap();

    let output = gigsift_with_data(data.path(), &["history", "--json"]);

    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(validator("history").is_valid(&value));
}

#[test]
fn changes_without_a_snapshot_says_so() {
    let dir = fixture();
    let root = resolved_root(&dir);
    let data = tempdir().unwrap();

    let output = gigsift_with_data(data.path(), &["changes", root.to_str().unwrap(), "--json"]);

    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(validator("changes").is_valid(&value));
    assert_eq!(value["data"]["baseline"], Value::Null);
    assert_eq!(value["warnings"][0]["code"], "no_baseline");
}

#[test]
fn a_snapshot_becomes_the_baseline_for_changes() {
    let dir = fixture();
    let root = resolved_root(&dir);
    let data = tempdir().unwrap();
    let text = root.to_str().unwrap();

    let taken = gigsift_with_data(data.path(), &["snapshot", text, "--json"]);
    assert!(taken.status.success(), "{taken:?}");
    let taken: Value = serde_json::from_slice(&taken.stdout).unwrap();
    assert!(validator("snapshot").is_valid(&taken));

    fs::write(root.join("media/new.mov"), vec![b'x'; 1_000_000]).unwrap();

    let output = gigsift_with_data(data.path(), &["changes", text, "--since", "1d", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(validator("changes").is_valid(&value));
    assert_eq!(
        value["data"]["baseline"]["snapshot_id"],
        taken["data"]["snapshot_id"]
    );
    assert_eq!(value["data"]["baseline"]["newer_than_requested"], true);
    assert_eq!(value["warnings"][0]["code"], "baseline_too_recent");
    assert!(value["data"]["allocated_delta_bytes"].as_i64().unwrap() >= 1_000_000);
}

#[test]
fn a_forecast_needs_history_and_says_so() {
    let dir = fixture();
    let root = resolved_root(&dir);
    let data = tempdir().unwrap();

    let output = gigsift_with_data(data.path(), &["forecast", root.to_str().unwrap(), "--json"]);

    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(validator("forecast").is_valid(&value));
    assert_eq!(value["data"]["outlook"], "insufficient_data");
    assert!(value["data"]["volume"]["total_bytes"].as_u64().unwrap() > 0);
    assert!(value["data"]["volume"]["available_bytes"].as_u64().unwrap() > 0);
}

/// Moves a fixture to the real Trash, so only CI runners opt in.
#[test]
fn a_full_run_moves_to_the_real_trash_and_records_it() {
    if std::env::var_os("GIGSIFT_TEST_REAL_TRASH").is_none() {
        return;
    }
    let dir = fixture();
    let root = resolved_root(&dir);
    let data = tempdir().unwrap();
    let id = saved_plan(&root, data.path());

    let output = gigsift_with_data(data.path(), &["clean", "apply", &id, "--yes", "--json"]);

    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(validator("apply").is_valid(&value));
    assert_eq!(value["data"]["moved_count"], 1);
    assert_eq!(value["data"]["failed_count"], 0);
    assert!(!root.join("app/node_modules").exists());
    assert!(root.join("app/src/main.ts").exists());
    assert!(root.join("app/.env").exists());
    let location = value["data"]["results"][0]["trash_location"]
        .as_str()
        .unwrap();
    assert!(Path::new(location).exists(), "{location}");

    let history = gigsift_with_data(data.path(), &["history", "--json"]);
    let value: Value = serde_json::from_slice(&history.stdout).unwrap();
    assert!(validator("history").is_valid(&value));
    assert_eq!(value["data"]["actions"][0]["status"], "moved");
}

#[test]
fn json_failures_use_the_error_contract() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("nope");

    let output = gigsift(&["scan", missing.to_str().unwrap(), "--json"]);

    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(validator("error").is_valid(&value));
    assert_eq!(value["error"]["code"], "root_not_readable");
}

#[test]
fn text_output_shows_size_and_top_entries() {
    let dir = fixture();
    let root = dir.path().to_str().unwrap();

    let output = gigsift(&["scan", root]);

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("On disk"), "{text}");
    assert!(text.contains("Largest folders"), "{text}");
}

fn scan_with_env(root: &str, flag: Option<&str>, key: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gigsift"));
    command.arg("scan").arg(root).args(flag);
    command
        .env_remove("CLICOLOR_FORCE")
        .env_remove("NO_COLOR")
        .env(key, "1");
    command.output().unwrap()
}

#[test]
fn piped_text_keeps_real_paths_and_no_styling() {
    let dir = fixture();
    let root = dir.path().to_str().unwrap();

    let output = scan_with_env(root, None, "GIGSIFT_UNSET_FOR_TEST");

    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains('\x1b'), "{text}");
    assert!(!text.contains('█'), "{text}");
    let media = dir.path().join("media").display().to_string();
    assert!(text.contains(&media), "{text}");
}

#[test]
fn forcing_color_styles_text_but_never_json() {
    let dir = fixture();
    let root = dir.path().to_str().unwrap();

    let text = scan_with_env(root, None, "CLICOLOR_FORCE").stdout;
    let json = scan_with_env(root, Some("--json"), "CLICOLOR_FORCE").stdout;

    assert!(String::from_utf8(text).unwrap().contains('\x1b'));
    assert!(!String::from_utf8_lossy(&json).contains('\x1b'));
    assert!(serde_json::from_slice::<Value>(&json).is_ok());
}

#[test]
fn piped_help_is_the_plain_wordmark_with_no_animation() {
    let output = gigsift(&["--help"]);
    let text = String::from_utf8(output.stdout).unwrap();

    assert!(output.status.success());
    assert!(text.contains("█▀▀█ ▀ █▀▀█ █▀▀▀ ▀ █▀▀▀ █▀▀\n"));
    assert!(!text.contains('\x1b'), "{text:?}");
}

#[test]
fn invalid_sizes_are_rejected() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let output = gigsift(&["top", root, "--min-size", "lots"]);
    assert!(!output.status.success());
}
