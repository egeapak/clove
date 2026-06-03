//! T-M04: `clove export json` / `clove export jsonl` end-to-end tests.

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

/// Init a repo and create a small set of items with labels, deps, and bodies.
fn init_with_items() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    clove(dir.path())
        .args(["new", "First", "-l", "area:core", "-b", "the body of first"])
        .assert()
        .success();
    clove(dir.path())
        .args(["new", "Second", "--type", "bug", "-p", "0"])
        .assert()
        .success();
    clove(dir.path())
        .args(["new", "Third", "-p", "2"])
        .assert()
        .success();

    // Add a dependency so blocked_by/ready are exercised.
    let ids = item_ids(dir.path());
    clove(dir.path())
        .args(["dep", "add", &ids[0], &ids[1]])
        .assert()
        .success();
    dir
}

fn item_ids(dir: &Path) -> Vec<String> {
    let out = clove(dir)
        .args(["ls", "--format", "json", "--limit", "0"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}

fn item_count(dir: &Path) -> usize {
    item_ids(dir).len()
}

#[test]
fn jsonl_every_line_is_standalone_json_and_count_matches() {
    let dir = init_with_items();
    let out = clove(dir.path())
        .args(["export", "jsonl"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), item_count(dir.path()));
    for line in &lines {
        let v: Value = serde_json::from_str(line).expect("each line is valid JSON");
        assert!(v.get("id").is_some(), "line is a bare item object: {v}");
        assert!(v.get("ok").is_none(), "no envelope wrapper: {v}");
    }
    // Exactly one trailing newline after the last record (no extra blank line).
    assert!(text.ends_with("}\n"));
    assert!(!text.ends_with("\n\n"));
}

#[test]
fn json_envelope_validates_against_item_list_schema() {
    let dir = init_with_items();
    let list = schema("item-list.json");
    let item = schema("item.json");

    let out = clove(dir.path()).args(["export", "json"]).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["v"], 1);
    let data = v["data"].as_array().unwrap();
    assert_eq!(data.len(), item_count(dir.path()));

    if let Err(e) = list.validate(&v) {
        panic!("list schema violation: {e}");
    }
    for element in data {
        if let Err(e) = item.validate(element) {
            panic!("item schema violation: {e} in {element}");
        }
    }
}

#[test]
fn export_is_deterministic_byte_for_byte() {
    let dir = init_with_items();
    let a = clove(dir.path())
        .args(["export", "jsonl"])
        .output()
        .unwrap()
        .stdout;
    let b = clove(dir.path())
        .args(["export", "jsonl"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(a, b, "jsonl export must be byte-identical");

    let a = clove(dir.path())
        .args(["export", "json"])
        .output()
        .unwrap()
        .stdout;
    let b = clove(dir.path())
        .args(["export", "json"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(a, b, "json export must be byte-identical");
}

#[test]
fn out_file_written_and_stdout_empty() {
    let dir = init_with_items();
    let out_path = dir.path().join("dump.jsonl");
    let out = clove(dir.path())
        .args(["export", "jsonl", "--out"])
        .arg(&out_path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "stdout must be empty with --out");

    let file = std::fs::read_to_string(&out_path).unwrap();
    let stdout_equiv = clove(dir.path())
        .args(["export", "jsonl"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(file.as_bytes(), stdout_equiv.as_slice());
    assert_eq!(file.lines().count(), item_count(dir.path()));
}

#[test]
fn out_file_json_written_and_stdout_empty() {
    let dir = init_with_items();
    let out_path = dir.path().join("dump.json");
    let out = clove(dir.path())
        .args(["export", "json", "--out"])
        .arg(&out_path)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "stdout must be empty with --out");
    let v: Value = serde_json::from_slice(&std::fs::read(&out_path).unwrap()).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"].as_array().unwrap().len(), item_count(dir.path()));
}

#[test]
fn empty_repo_jsonl_zero_lines_json_empty_data() {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();

    let out = clove(dir.path())
        .args(["export", "jsonl"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "empty repo → zero jsonl lines");

    let out = clove(dir.path()).args(["export", "json"]).output().unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"], serde_json::json!([]));
}

#[test]
fn export_github_with_out_is_rejected() {
    // `--out` is a file sink; `export github` is a network sink. Passing both
    // must be a clean validation error (exit 4), never a silently-ignored flag.
    let dir = init_with_items();
    let out_path = dir.path().join("dump.json");
    let out = clove(dir.path())
        .args(["export", "github", "owner/repo", "--out"])
        .arg(&out_path)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(4),
        "export github --out must be a validation error (exit 4)"
    );
    assert!(
        !out_path.exists(),
        "the --out file must not have been written"
    );
}

#[test]
fn export_json_with_target_is_rejected() {
    // An `owner/repo` target is only meaningful for `export github`; passing it
    // to json/jsonl must be a clean validation error (exit 4), not ignored.
    let dir = init_with_items();
    for fmt in ["json", "jsonl"] {
        let out = clove(dir.path())
            .args(["export", fmt, "owner/repo"])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(4),
            "export {fmt} <target> must be a validation error (exit 4)"
        );
    }
}

#[test]
fn exported_item_includes_full_frontmatter_fields() {
    let dir = init_with_items();
    let out = clove(dir.path())
        .args(["export", "jsonl"])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    // The first-created item carries a label and a body — assert the full shape,
    // not the lean ls projection.
    let first: Value = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .find(|v| v["title"] == "First")
        .expect("First item exported");

    assert!(first.get("created").is_some(), "created present");
    assert!(first.get("updated").is_some(), "updated present");
    assert!(first.get("labels").is_some(), "labels present");
    assert_eq!(first["labels"], serde_json::json!(["area:core"]));
    assert!(first.get("deps").is_some(), "deps present");
    assert!(first.get("body").is_some(), "body present");
    assert_eq!(first["body"], "the body of first");
    assert!(first.get("relates").is_some(), "relates present");
    assert!(first.get("ready").is_some(), "computed ready present");
    assert!(
        first.get("blocked_by").is_some(),
        "computed blocked_by present"
    );
}
