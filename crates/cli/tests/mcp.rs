//! Talks to `gigsift mcp serve` over real stdio, the way an MCP client does.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

const TOOLS: [&str; 8] = [
    "create_cleanup_plan",
    "explain_storage_path",
    "find_cleanup_candidates",
    "find_large_files",
    "get_project_storage",
    "get_storage_changes",
    "get_storage_forecast",
    "get_storage_summary",
];

struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    data: PathBuf,
}

/// A home folder holding one project, standing in for the real one.
fn fixture() -> Fixture {
    let dir = tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let text = root.to_string_lossy();
    let root = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));
    let home = root.join("home-of-someone");
    fs::create_dir_all(home.join("code/app/node_modules/dep")).unwrap();
    fs::write(home.join("code/app/package.json"), "{}").unwrap();
    fs::write(
        home.join("code/app/node_modules/dep/index.js"),
        vec![b'x'; 200_000],
    )
    .unwrap();
    Fixture {
        _dir: dir,
        data: root.join("data"),
        home,
    }
}

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start(fixture: &Fixture, args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gigsift"))
            .args(["mcp", "serve"])
            .args(args)
            .env("HOME", &fixture.home)
            .env("USERPROFILE", &fixture.home)
            .env("GIGSIFT_DATA_DIR", &fixture.data)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let initialized = client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "gigsift-test", "version": "1" }
            }),
        );
        assert_eq!(initialized["serverInfo"]["name"], "gigsift");
        assert_eq!(
            initialized["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        client
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Every line on stdout must be protocol JSON, or this fails to parse.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).unwrap();
            assert!(read > 0, "server closed the stream");
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == json!(id) {
                assert!(message.get("error").is_none(), "{message}");
                return message["result"].clone();
            }
        }
    }

    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )
    }

    /// Parses a successful tool call's JSON payload.
    fn call_ok(&mut self, tool: &str, arguments: Value) -> Value {
        let result = self.call(tool, arguments);
        assert_ne!(result["isError"], json!(true), "{result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tool_names(client: &mut Client) -> Vec<String> {
    let listed = client.request("tools/list", json!({}));
    let mut names: Vec<String> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

fn modules(fixture: &Fixture) -> String {
    let path = fixture.home.join("code").join("app").join("node_modules");
    path.to_string_lossy().into_owned()
}

fn home_text(fixture: &Fixture) -> String {
    fixture.home.to_string_lossy().into_owned()
}

#[test]
fn the_server_offers_exactly_the_read_and_plan_tools() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);
    assert_eq!(tool_names(&mut client), TOOLS);
}

#[test]
fn no_tool_can_apply_or_remove_anything() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);
    for name in tool_names(&mut client) {
        for forbidden in ["apply", "delete", "remove", "trash", "clean_up", "move"] {
            assert!(!name.contains(forbidden), "{name}");
        }
    }
}

#[test]
fn every_tool_publishes_an_input_schema() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);
    let listed = client.request("tools/list", json!({}));
    for tool in listed["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["description"].as_str().unwrap().len() > 20, "{tool}");
    }
}

#[test]
fn explain_works_and_hides_home_by_default() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);

    let report = client.call_ok("explain_storage_path", json!({ "path": modules(&fixture) }));

    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["data"]["recognized"]["recipe_id"], "node-modules");
    assert!(report["data"]["path"].as_str().unwrap().starts_with('~'));
    assert!(!report.to_string().contains(&home_text(&fixture)));
    assert!(report.get("scan_completeness").is_some());
}

#[test]
fn hidden_sharing_keeps_only_final_names() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &["--sharing", "hidden"]);

    let report = client.call_ok("explain_storage_path", json!({ "path": modules(&fixture) }));

    assert_eq!(report["data"]["path"], "<hidden>/node_modules");
    assert!(!report.to_string().contains("home-of-someone"));
    assert!(!report.to_string().contains("code"));
}

#[test]
fn full_sharing_is_available_when_asked_for() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &["--sharing", "full"]);

    let report = client.call_ok("explain_storage_path", json!({ "path": modules(&fixture) }));

    assert_eq!(report["data"]["path"], modules(&fixture));
}

#[test]
fn summary_and_large_files_default_to_home() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);

    let summary = client.call_ok("get_storage_summary", json!({}));
    assert_eq!(summary["data"]["root"], "~");
    assert_eq!(summary["data"]["totals"]["files"], 2);

    let large = client.call_ok(
        "find_large_files",
        json!({ "min_size": "100kb", "limit": 1 }),
    );
    assert_eq!(large["data"]["entries"].as_array().unwrap().len(), 1);
    assert!(large["data"]["truncated"].as_u64().unwrap() > 0);
}

#[test]
fn candidates_list_what_is_recoverable_and_save_nothing() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);

    let report = client.call_ok("find_cleanup_candidates", json!({ "active_within": "0d" }));

    let candidates = report["data"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["regenerate"], "npm install");
    assert!(report["data"].get("plan_id").is_none());
    assert!(!fixture.data.join("gigsift.db").exists());
}

#[test]
fn recently_changed_projects_are_reported_as_blocked() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);

    let report = client.call_ok("find_cleanup_candidates", json!({}));

    assert!(report["data"]["candidates"].as_array().unwrap().is_empty());
    assert_eq!(
        report["data"]["blocked"][0]["blocked_by"][0],
        "recent_activity"
    );
}

#[test]
fn project_storage_lists_artifacts() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);

    let report = client.call_ok("get_project_storage", json!({ "active_within": "0d" }));

    let project = &report["data"]["projects"][0];
    assert_eq!(project["artifacts"][0]["recipe_id"], "node-modules");
    assert_eq!(project["root"], "~/code/app".replace('/', &separator()));
}

fn separator() -> String {
    std::path::MAIN_SEPARATOR.to_string()
}

fn gigsift(fixture: &Fixture, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_gigsift"))
        .args(args)
        .env("GIGSIFT_DATA_DIR", &fixture.data)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_plan_made_by_an_agent_is_saved_for_a_person_and_nothing_moves() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);

    let plan = client.call_ok(
        "create_cleanup_plan",
        json!({ "target": "1kb", "active_within": "0d" }),
    );

    let id = plan["data"]["plan_id"].as_str().unwrap();
    assert!(plan["data"]["target_met"].as_bool().unwrap());
    assert!(Path::new(&modules(&fixture)).exists());

    let shown = gigsift(&fixture, &["clean", "show", id, "--json"]);
    assert!(shown.status.success(), "{shown:?}");
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["data"]["plan_id"], id);
}

#[test]
fn a_bad_call_is_a_tool_error_and_the_server_keeps_going() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);

    let missing = client.call("explain_storage_path", json!({ "path": "/no/such/path" }));
    assert_eq!(missing["isError"], json!(true), "{missing}");

    let bad_size = client.call("create_cleanup_plan", json!({ "target": "lots" }));
    assert_eq!(bad_size["isError"], json!(true), "{bad_size}");

    assert_eq!(tool_names(&mut client).len(), TOOLS.len());
}

#[test]
fn changes_explain_how_to_start_when_there_is_no_snapshot() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);

    let report = client.call_ok("get_storage_changes", json!({}));

    assert_eq!(report["data"]["baseline"], Value::Null);
    assert_eq!(report["warnings"][0]["code"], "no_baseline");
}

#[test]
fn changes_compare_with_a_snapshot_taken_by_the_cli() {
    let fixture = fixture();
    let root = fixture.home.to_string_lossy().into_owned();
    let taken = gigsift(&fixture, &["snapshot", &root, "--json"]);
    assert!(taken.status.success(), "{taken:?}");
    fs::write(fixture.home.join("grew.bin"), vec![b'x'; 1_000_000]).unwrap();

    let mut client = Client::start(&fixture, &[]);
    let report = client.call_ok(
        "get_storage_changes",
        json!({ "path": root, "since": "1d" }),
    );

    assert!(report["data"]["allocated_delta_bytes"].as_i64().unwrap() >= 1_000_000);
}

#[test]
fn a_forecast_says_when_it_has_too_little_history() {
    let fixture = fixture();
    let mut client = Client::start(&fixture, &[]);

    let report = client.call_ok("get_storage_forecast", json!({}));

    assert_eq!(report["data"]["outlook"], "insufficient_data");
}

#[test]
fn the_cli_lists_candidates_without_saving_anything() {
    let fixture = fixture();
    let output = gigsift(
        &fixture,
        &[
            "clean",
            "candidates",
            fixture.home.to_str().unwrap(),
            "--active-within",
            "0d",
            "--json",
        ],
    );

    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["command"], "clean candidates");
    assert_eq!(value["data"]["candidates"].as_array().unwrap().len(), 1);
    assert!(!fixture.data.join("gigsift.db").exists());
}

#[test]
fn cli_json_can_be_redacted_for_ai_tools_too() {
    let fixture = fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_gigsift"))
        .args(["explain", &modules(&fixture), "--json"])
        .args(["--share-paths", "hidden"])
        .env("HOME", &fixture.home)
        .env("USERPROFILE", &fixture.home)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["path"], "<hidden>/node_modules");
}
