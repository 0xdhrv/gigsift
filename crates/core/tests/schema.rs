//! Golden guard for the published contract. If this fails, either the change is
//! additive and `/schema` should be regenerated with `UPDATE_GOLDEN=1`, or it is
//! breaking and `SCHEMA_VERSION` must be bumped first.

use std::path::{Path, PathBuf};

use gigsift_core::{Candidate, Envelope, ErrorResponse, SCHEMA_VERSION, ScanData, TopData};
use schemars::{JsonSchema, schema_for};
use serde_json::{Value, json};

fn schema_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schema")
        .join(format!("v{SCHEMA_VERSION}"))
}

fn check_schema<T: JsonSchema>(name: &str) {
    let generated = serde_json::to_value(schema_for!(T)).unwrap();
    let path = schema_dir().join(format!("{name}.schema.json"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        let text = serde_json::to_string_pretty(&generated).unwrap() + "\n";
        std::fs::write(&path, text).unwrap();
    }
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let committed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        committed,
        generated,
        "{name} schema drifted from {}",
        path.display()
    );
}

#[test]
fn published_schemas_match_the_types() {
    check_schema::<Envelope<ScanData>>("scan");
    check_schema::<Envelope<TopData>>("top");
    check_schema::<Candidate>("candidate");
    check_schema::<ErrorResponse>("error");
}

fn candidate_example() -> Value {
    json!({
        "schema_version": 1,
        "candidate_id": "cand_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "path": "/Users/me/code/app/node_modules",
        "category": "developer_dependency",
        "project": {
            "root": "/Users/me/code/app",
            "active": false,
            "last_activity": "2026-09-12T10:00:00Z"
        },
        "git_status": "clean",
        "logical_bytes": 2400000000u64,
        "allocated_bytes": 2310000000u64,
        "reclaimable_bytes_estimate": 2290000000u64,
        "reason": "Reinstallable dependencies for an inactive project",
        "loses": "Installed packages; no source files",
        "regenerate": "npm install",
        "rebuild_cost": { "time": "minutes", "network": "high" },
        "risk": "low",
        "confidence": 0.94,
        "recommended_action": "move_to_trash",
        "requires_confirmation": true
    })
}

fn candidate_validator() -> jsonschema::Validator {
    let schema = serde_json::to_value(schema_for!(Candidate)).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

#[test]
fn candidate_contract_round_trips() {
    let example = candidate_example();
    let validator = candidate_validator();
    assert!(validator.is_valid(&example));

    let parsed: Candidate = serde_json::from_value(example.clone()).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), example);
}

#[test]
fn candidate_schema_rejects_actions_outside_the_allow_list() {
    let mut example = candidate_example();
    example["recommended_action"] = json!("delete");
    assert!(!candidate_validator().is_valid(&example));
}

#[test]
fn candidate_schema_rejects_out_of_range_confidence() {
    let mut example = candidate_example();
    example["confidence"] = json!(1.5);
    assert!(!candidate_validator().is_valid(&example));
}
