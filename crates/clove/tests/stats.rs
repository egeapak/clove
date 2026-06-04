//! M4: `clove stats` end-to-end tests — analytics shape, schema, persistence.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use jsonschema::Validator;
use serde_json::Value;
use tempfile::TempDir;

fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

fn schema(name: &str) -> Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/json-schema/v1")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    jsonschema::validator_for(&value).expect("valid schema")
}

fn json(cmd: &mut Command) -> Value {
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "command failed: {out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

fn init_with_items() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    clove(dir.path())
        .args([
            "new",
            "First",
            "--type",
            "feature",
            "-p",
            "1",
            "-a",
            "alice",
            "-l",
            "area:core",
        ])
        .assert()
        .success();
    clove(dir.path())
        .args([
            "new",
            "Second",
            "--type",
            "bug",
            "-a",
            "alice",
            "-l",
            "area:core",
        ])
        .assert()
        .success();
    clove(dir.path())
        .args(["new", "Third", "--type", "docs"])
        .assert()
        .success();

    // First blocks Second (Second depends on First, which is open).
    let ids = item_ids(dir.path());
    clove(dir.path())
        .args(["dep", "add", &ids[1], &ids[0]])
        .assert()
        .success();
    dir
}

fn item_ids(dir: &Path) -> Vec<String> {
    let v = json(clove(dir).args(["ls", "--format", "json", "--limit", "0"]));
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn stats_json_validates_against_schema() {
    let dir = init_with_items();
    let stats = schema("stats.json");

    let v = json(clove(dir.path()).args(["stats", "--format", "json"]));
    if let Err(e) = stats.validate(&v) {
        panic!("stats schema violation: {e}");
    }

    let data = &v["data"];
    assert_eq!(data["total"], 3);
    assert_eq!(data["by_status"]["open"], 3);
    assert_eq!(data["by_type"]["bug"], 1);
    assert_eq!(data["by_type"]["feature"], 1);
    assert_eq!(data["by_type"]["docs"], 1);
    // One open dep (First) blocks Second; First and Third are ready.
    assert_eq!(data["ready"], 2, "{data}");
    assert_eq!(data["blocked"], 1, "{data}");
    assert_eq!(data["unassigned"], 1);
    assert_eq!(data["daemon"]["running"], false);
}

#[test]
fn stats_on_empty_repo() {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    let v = json(clove(dir.path()).args(["stats", "--format", "json"]));
    assert_eq!(v["data"]["total"], 0);
    assert_eq!(v["data"]["ready"], 0);
    assert!(v["data"]["epics"].as_array().unwrap().is_empty());
}

#[test]
fn snapshot_persists_and_history_reads_back() {
    let dir = init_with_items();

    // No history yet.
    let empty = json(clove(dir.path()).args(["stats", "--history", "--format", "json"]));
    assert_eq!(empty["data"].as_array().unwrap().len(), 0);

    // Record a snapshot; the durable store appears.
    clove(dir.path())
        .args(["stats", "--snapshot"])
        .assert()
        .success();
    assert!(dir.path().join(".clove/stats.db").exists());

    let hist = json(clove(dir.path()).args(["stats", "--history", "--format", "json"]));
    let rows = hist["data"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["stats"]["total"], 3);
    assert!(rows[0]["captured_at"].is_string());

    // A second snapshot accumulates.
    clove(dir.path())
        .args(["stats", "--snapshot"])
        .assert()
        .success();
    let hist2 = json(clove(dir.path()).args(["stats", "--history", "--format", "json"]));
    assert_eq!(hist2["data"].as_array().unwrap().len(), 2);

    // --limit caps the series.
    let limited =
        json(clove(dir.path()).args(["stats", "--history", "--limit", "1", "--format", "json"]));
    assert_eq!(limited["data"].as_array().unwrap().len(), 1);
}

#[test]
fn history_since_filters_by_timestamp() {
    let dir = init_with_items();
    clove(dir.path())
        .args(["stats", "--snapshot"])
        .assert()
        .success();

    // A far-future `--since` excludes the just-recorded snapshot.
    let future = json(clove(dir.path()).args([
        "stats",
        "--history",
        "--since",
        "2999-01-01T00:00:00+00:00",
        "--format",
        "json",
    ]));
    assert_eq!(future["data"].as_array().unwrap().len(), 0);

    // A past `--since` includes it.
    let past = json(clove(dir.path()).args([
        "stats",
        "--history",
        "--since",
        "2000-01-01T00:00:00+00:00",
        "--format",
        "json",
    ]));
    assert_eq!(past["data"].as_array().unwrap().len(), 1);
}

#[test]
fn top_caps_breakdowns() {
    let dir = init_with_items();
    // Both labeled items share area:core; cap doesn't drop it, but `--top 1`
    // limits the assignee list to one row.
    let v = json(clove(dir.path()).args(["stats", "--top", "1", "--format", "json"]));
    assert!(v["data"]["by_assignee"].as_array().unwrap().len() <= 1);
}
