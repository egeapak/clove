//! T-CLI14: assert command JSON output validates against the published v1 schema.

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
    cmd
}

/// Compile a schema from `docs/json-schema/v1/<name>`.
fn schema(name: &str) -> Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/json-schema/v1")
        .join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let value: Value = serde_json::from_str(&text).unwrap();
    jsonschema::validator_for(&value).expect("valid schema")
}

fn assert_valid(validator: &Validator, instance: &Value) {
    if let Err(error) = validator.validate(instance) {
        panic!("schema violation: {error} in {instance}");
    }
}

fn run_json(cmd: &mut Command) -> (Value, i32) {
    let out = cmd.arg("--format").arg("json").output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    (v, out.status.code().unwrap_or(-1))
}

fn init_with_items() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    clove(dir.path())
        .args(["new", "First", "-l", "area:core"])
        .assert()
        .success();
    clove(dir.path())
        .args(["new", "Second", "--type", "bug", "-p", "0"])
        .assert()
        .success();
    dir
}

#[test]
fn ls_output_matches_item_list_schema() {
    let dir = init_with_items();
    let item = schema("item.json");
    let list = schema("item-list.json");

    let (v, code) = run_json(clove(dir.path()).arg("ls"));
    assert_eq!(code, 0);
    assert_valid(&list, &v);
    for element in v["data"].as_array().unwrap() {
        assert_valid(&item, element);
    }
}

#[test]
fn show_output_matches_item_schema() {
    let dir = init_with_items();
    let item = schema("item.json");

    let ls = run_json(clove(dir.path()).arg("ls")).0;
    let id = ls["data"][0]["id"].as_str().unwrap().to_owned();

    let (v, code) = run_json(clove(dir.path()).args(["show", &id]));
    assert_eq!(code, 0);
    assert_valid(&item, &v["data"]);
}

#[test]
fn not_found_matches_error_schema() {
    let dir = init_with_items();
    let error = schema("error.json");

    let (v, code) = run_json(clove(dir.path()).args(["show", "proj-ZZZZZZZZ"]));
    assert_eq!(code, 2);
    assert_valid(&error, &v);
}

/// Create an item and return its id.
fn new_item(dir: &std::path::Path, title: &str) -> String {
    let (v, _) = run_json(clove(dir).args(["new", title]));
    v["data"]["id"].as_str().unwrap().to_owned()
}

#[test]
fn ready_output_matches_item_list_schema() {
    let dir = init_with_items();
    let list = schema("item-list.json");
    let (v, code) = run_json(clove(dir.path()).arg("ready"));
    assert_eq!(code, 0);
    assert_valid(&list, &v);
}

#[test]
fn index_ls_lean_matches_item_list_schema() {
    let dir = init_with_items();
    clove(dir.path()).arg("reindex").assert().success();
    let list = schema("item-list.json");
    let (v, code) = run_json(clove(dir.path()).arg("ls"));
    assert_eq!(code, 0);
    // The index path returns the lean projection (no created/updated); it must
    // still satisfy the list schema, which requires only the lean fields.
    assert_eq!(v["_meta"]["source"], "index");
    assert_valid(&list, &v);
}

#[test]
fn dep_tree_matches_schema() {
    let dir = init_with_items();
    let root = new_item(dir.path(), "Root");
    let dep = new_item(dir.path(), "Dep");
    clove(dir.path())
        .args(["dep", "add", &root, &dep])
        .assert()
        .success();

    let tree = schema("dep-tree.json");
    let (v, code) = run_json(clove(dir.path()).args(["dep", "tree", &root]));
    assert_eq!(code, 0);
    assert_valid(&tree, &v);
}

#[test]
fn comments_match_schema() {
    let dir = init_with_items();
    let id = new_item(dir.path(), "Discussed");
    clove(dir.path())
        .args(["comment", &id, "a note"])
        .assert()
        .success();

    let comments = schema("comment-list.json");
    let (v, code) = run_json(clove(dir.path()).args(["comments", &id]));
    assert_eq!(code, 0);
    assert_valid(&comments, &v);
}
