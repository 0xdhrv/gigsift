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
    dir
}

/// Replaces machine-specific values, leaving the shape and every logical size.
fn normalize(value: &mut Value, root: &str) {
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                match key.as_str() {
                    "scanned_at" => *item = json!("<timestamp>"),
                    "allocated_bytes" => *item = json!("<allocated>"),
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
    assert!(
        validator(name.split('-').next().unwrap()).is_valid(&value),
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

#[test]
fn invalid_sizes_are_rejected() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let output = gigsift(&["top", root, "--min-size", "lots"]);
    assert!(!output.status.success());
}
