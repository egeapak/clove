//! End-to-end CLI tests for the M0 command surface and index wiring.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A `clove` invocation rooted at `dir`, with a clean environment.
fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

/// Initialize a repo in a fresh temp dir and return it.
fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    dir
}

/// Run a command expecting JSON success and return the parsed envelope.
fn json_ok(cmd: &mut Command) -> Value {
    let out = cmd.arg("--format").arg("json").output().unwrap();
    assert!(out.status.success(), "command failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["ok"], true, "envelope not ok: {v}");
    v
}

/// Create an item and return its id.
fn new_item(dir: &Path, title: &str, extra: &[&str]) -> String {
    let mut cmd = clove(dir);
    cmd.arg("new").arg(title).args(extra);
    let v = json_ok(&mut cmd);
    v["data"]["id"].as_str().unwrap().to_owned()
}

#[test]
fn init_is_idempotent_and_writes_gitignore() {
    let dir = init_repo();
    // Second init does not fail and does not overwrite config.
    clove(dir.path()).arg("init").assert().success();

    let gitignore = std::fs::read_to_string(dir.path().join(".clove/.gitignore")).unwrap();
    for entry in [
        "index.db",
        "*.db-shm",
        "*.db-wal",
        "daemon.sock",
        "daemon.pid",
        "reindex.lock",
        "daemon.lock",
        "index.db.tmp",
    ] {
        assert!(gitignore.contains(entry), "missing {entry}");
    }
    assert!(!gitignore.contains('\r'), "gitignore must use LF endings");
    assert!(dir.path().join(".clove/config.toml").exists());
}

#[test]
fn new_show_round_trip() {
    let dir = init_repo();
    let id = new_item(dir.path(), "A task", &["--type", "bug", "-p", "1"]);

    let v = json_ok(clove(dir.path()).arg("show").arg(&id));
    assert_eq!(v["data"]["id"], id);
    assert_eq!(v["data"]["type"], "bug");
    assert_eq!(v["data"]["priority"], 1);
    assert_eq!(v["data"]["status"], "open");
}

#[test]
fn ready_and_blocked_partition_by_dependency() {
    let dir = init_repo();
    let dep = new_item(dir.path(), "Dependency", &[]);
    let blocked = new_item(dir.path(), "Dependent", &["--dep", &dep]);

    // The dependent is blocked; the dependency is ready.
    let ready = json_ok(clove(dir.path()).arg("ready"));
    let ready_ids: Vec<&str> = ready["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(ready_ids.contains(&dep.as_str()));
    assert!(!ready_ids.contains(&blocked.as_str()));

    let blk = json_ok(clove(dir.path()).arg("blocked"));
    let blk_ids: Vec<&str> = blk["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(blk_ids.contains(&blocked.as_str()));

    // Closing the dependency makes the dependent ready.
    clove(dir.path()).arg("close").arg(&dep).assert().success();
    let ready2 = json_ok(clove(dir.path()).arg("ready"));
    let ready2_ids: Vec<&str> = ready2["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert!(ready2_ids.contains(&blocked.as_str()));
}

#[test]
fn close_sets_then_clears_closed_timestamp() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Closable", &[]);

    let closed = json_ok(clove(dir.path()).arg("close").arg(&id));
    assert_eq!(closed["data"]["status"], "closed");
    assert!(closed["data"]["closed"].is_string());

    let reopened = json_ok(clove(dir.path()).args(["status", &id, "open"]));
    assert_eq!(reopened["data"]["status"], "open");
    assert!(reopened["data"]["closed"].is_null());
}

#[test]
fn labels_are_canonicalized_and_filterable() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Labeled", &[]);

    clove(dir.path())
        .args(["label", &id, "add", "Area:iOS"])
        .assert()
        .success();
    // Adding the canonical form again is a no-op (single label).
    let v = json_ok(clove(dir.path()).args(["label", &id, "add", "area:ios"]));
    assert_eq!(v["data"]["labels"], serde_json::json!(["area:ios"]));

    // Filter matches regardless of input case.
    let ls = json_ok(clove(dir.path()).args(["ls", "--label", "AREA:IOS"]));
    assert_eq!(ls["data"].as_array().unwrap().len(), 1);

    // Remove with a non-canonical argument.
    let removed = json_ok(clove(dir.path()).args(["label", &id, "rm", "AREA:IOS"]));
    assert_eq!(removed["data"]["labels"], serde_json::json!([]));
}

#[test]
fn priority_out_of_range_exits_4() {
    let dir = init_repo();
    let id = new_item(dir.path(), "P", &[]);
    clove(dir.path())
        .args(["priority", &id, "5"])
        .assert()
        .failure()
        .code(4);
}

#[test]
fn dep_validation_exit_codes() {
    let dir = init_repo();
    let a = new_item(dir.path(), "A", &[]);
    let b = new_item(dir.path(), "B", &[]);

    // self-dependency → exit 4
    clove(dir.path())
        .args(["dep", "add", &a, &a])
        .assert()
        .failure()
        .code(4);

    // missing dependency target → exit 2
    clove(dir.path())
        .args(["dep", "add", &a, "proj-ZZZZZZZZ"])
        .assert()
        .failure()
        .code(2);

    // a → b, then b → a would cycle → exit 3
    clove(dir.path())
        .args(["dep", "add", &a, &b])
        .assert()
        .success();
    clove(dir.path())
        .args(["dep", "add", &b, &a])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn show_missing_item_json_error_envelope() {
    let dir = init_repo();
    let out = clove(dir.path())
        .args(["show", "proj-ZZZZZZZZ", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "ITEM_NOT_FOUND");
    assert_eq!(v["error"]["exit"], 2);
}

#[test]
fn reindex_then_search_uses_index() {
    let dir = init_repo();
    new_item(
        dir.path(),
        "Findable widget",
        &["-b", "the body mentions sprockets"],
    );
    new_item(dir.path(), "Other", &[]);

    clove(dir.path()).arg("reindex").assert().success();

    let v = json_ok(clove(dir.path()).args(["search", "sprockets"]));
    assert_eq!(v["_meta"]["source"], "index");
    assert_eq!(v["data"].as_array().unwrap().len(), 1);

    // Without an index (forced), it falls back to a file scan.
    let v2 = json_ok(clove(dir.path()).args(["search", "widget", "--no-index"]));
    assert_eq!(v2["_meta"]["source"], "files");
    assert_eq!(v2["data"].as_array().unwrap().len(), 1);
}

#[test]
fn comment_add_then_list() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Discussed", &[]);
    clove(dir.path())
        .args(["comment", &id, "first note"])
        .assert()
        .success();
    let v = json_ok(clove(dir.path()).args(["comments", &id]));
    let comments = v["data"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["body"], "first note");
    // The author is stored as a filename-safe slug derived from the email.
    assert!(comments[0]["author"].as_str().unwrap().contains("tester"));
}

#[test]
fn agent_doc_is_idempotent_and_checks_schema() {
    let dir = init_repo();
    let a = clove(dir.path()).arg("agent-doc").output().unwrap();
    let b = clove(dir.path()).arg("agent-doc").output().unwrap();
    assert_eq!(a.stdout, b.stdout, "agent-doc must be byte-identical");

    let doc_path = dir.path().join("AGENTS.md");
    std::fs::write(&doc_path, &a.stdout).unwrap();
    clove(dir.path())
        .args(["agent-doc", "--check", "--file"])
        .arg(&doc_path)
        .assert()
        .success();
}

#[test]
fn doctor_reports_and_fixes_safe_issues() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Has issues", &[]);

    // Seed a non-canonical label by editing the file directly, and an orphan
    // comment directory.
    let item_path = dir.path().join(format!(".clove/issues/{id}.md"));
    let contents = std::fs::read_to_string(&item_path).unwrap();
    let contents = contents.replace("labels: []", "labels:\n  - Area:iOS");
    std::fs::write(&item_path, contents).unwrap();
    std::fs::create_dir_all(dir.path().join(".clove/issues/proj-ORPHAN00/comments")).unwrap();

    // doctor reports two fixable warnings.
    let report = json_ok(clove(dir.path()).arg("doctor"));
    assert!(report["data"]["summary"]["warnings"].as_u64().unwrap() >= 1);

    // --fix resolves them; a subsequent run is clean.
    clove(dir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();
    let after = json_ok(clove(dir.path()).arg("doctor"));
    assert_eq!(after["data"]["summary"]["warnings"], 0);
    assert_eq!(after["data"]["summary"]["errors"], 0);
    assert!(!dir.path().join(".clove/issues/proj-ORPHAN00").exists());
}

#[test]
fn doctor_strict_exits_4_on_errors() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Dangling", &[]);
    // Introduce a dangling dependency by hand-editing.
    let item_path = dir.path().join(format!(".clove/issues/{id}.md"));
    let contents = std::fs::read_to_string(&item_path).unwrap();
    let contents = contents.replace("deps: []", "deps:\n  - proj-MISSING0");
    std::fs::write(&item_path, contents).unwrap();

    clove(dir.path())
        .args(["doctor", "--strict"])
        .assert()
        .failure()
        .code(4);
}

#[test]
fn env_clove_format_json_without_flag() {
    let dir = init_repo();
    new_item(dir.path(), "Item", &[]);
    let out = clove(dir.path())
        .env("CLOVE_FORMAT", "json")
        .arg("ls")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
}
