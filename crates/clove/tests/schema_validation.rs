//! T-CLI14: assert command JSON output validates against the published v1 schema.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use jsonschema::Validator;
use serde_json::{json, Value};
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

#[test]
fn version_output_matches_schema() {
    let dir = init_with_items();
    let version = schema("version.json");
    let (v, code) = run_json(clove(dir.path()).arg("version"));
    assert_eq!(code, 0);
    assert_valid(&version, &v);
    // Sanity: the payload is the real version data, not an empty object.
    assert!(v["data"]["clove"].as_str().is_some());
    assert!(v["data"]["schema"].as_i64().is_some());
}

#[test]
fn reindex_output_matches_schema() {
    let dir = init_with_items();
    let reindex = schema("reindex.json");
    let (v, code) = run_json(clove(dir.path()).arg("reindex"));
    assert_eq!(code, 0);
    assert_valid(&reindex, &v);
    // The seeded store has two items; the rebuild must report them.
    assert_eq!(v["data"]["items_indexed"].as_i64().unwrap(), 2);
}

#[test]
fn new_output_matches_schema() {
    let dir = init_with_items();
    let new = schema("new.json");
    let (v, code) =
        run_json(clove(dir.path()).args(["new", "Fresh", "--type", "chore", "-p", "3"]));
    assert_eq!(code, 0);
    assert_valid(&new, &v);
    // The id must match the configured `proj` prefix.
    assert!(v["data"]["id"].as_str().unwrap().starts_with("proj-"));
}

#[test]
fn doctor_output_matches_schema() {
    let dir = init_with_items();
    let doctor = schema("doctor.json");

    // A clean store still produces a valid (empty-issues) envelope.
    let (clean, code) = run_json(clove(dir.path()).arg("doctor"));
    assert_eq!(code, 0);
    assert_valid(&doctor, &clean);

    // Seed a spread of findings across severities, fixability, and the store /
    // index check families so the `code` enum and issue shape are exercised:
    //   GITIGNORE_DRIFT (warning, fixable), ORPHAN_COMMENTS (warning, fixable),
    //   DANGLING_REF (error), INDEX_DIVERGENCE/INDEX_CORRUPT (index family).
    let id = new_item(dir.path(), "Has a dangling dep");
    let item = dir.path().join(format!(".clove/issues/{id}.md"));
    let body = std::fs::read_to_string(&item)
        .unwrap()
        .replace("deps: []", "deps: [proj-MISSING0]");
    std::fs::write(&item, body).unwrap();
    std::fs::create_dir_all(dir.path().join(".clove/issues/proj-GHOST000/comments")).unwrap();
    std::fs::write(dir.path().join(".clove/.gitignore"), "index.db\n").unwrap();
    clove(dir.path()).arg("reindex").assert().success();
    std::fs::write(dir.path().join(".clove/index.db"), b"not a database").unwrap();

    let (dirty, _) = run_json(clove(dir.path()).arg("doctor"));
    assert_valid(&doctor, &dirty);
    // Sanity: the seeded findings actually showed up (the schema is validating a
    // populated envelope, not an empty one).
    let codes: Vec<&str> = dirty["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    for expected in ["DANGLING_REF", "GITIGNORE_DRIFT", "ORPHAN_COMMENTS"] {
        assert!(codes.contains(&expected), "missing {expected} in {codes:?}");
    }
}

/// `_meta` has a real schema, not `{"type": "object"}` (read-path roadmap §7).
///
/// A bare object schema validates anything, so `_meta.limit` — whose `0` means
/// *unlimited*, the sort of thing a schema exists to state — was invisible to a
/// client generating types from the published document, and a typo'd or
/// wrong-typed key validated happily. The keys are now described and
/// `additionalProperties` is false, so a new `_meta` key has to be documented
/// here rather than shipping unannounced.
#[test]
fn meta_is_described_by_the_published_schemas() {
    let dir = init_with_items();
    let list = schema("item-list.json");
    let comments_schema = schema("comment-list.json");
    let stats_schema = schema("stats.json");

    // Every real envelope still validates.
    let ls = run_json(clove(dir.path()).arg("ls")).0;
    assert_valid(&list, &ls);
    let id = ls["data"][0]["id"].as_str().unwrap().to_owned();
    clove(dir.path())
        .args(["comment", &id, "a note"])
        .assert()
        .success();
    let comments = run_json(clove(dir.path()).args(["comments", &id])).0;
    assert_valid(&comments_schema, &comments);
    let stats = run_json(clove(dir.path()).arg("stats")).0;
    assert_valid(&stats_schema, &stats);

    // The keys the roadmap named are actually described, not just permitted.
    let ls_meta = ls["_meta"].as_object().expect("_meta object");
    for key in [
        "total", "returned", "offset", "limit", "sort", "dir", "source",
    ] {
        assert!(ls_meta.contains_key(key), "`ls` _meta lost {key}: {ls}");
    }
    assert_eq!(ls_meta["limit"], 100, "the CLI default cap is reported");

    // …and a wrong-typed or unknown `_meta` key is now a violation. Under the
    // old `{"type": "object"}` every one of these validated.
    let mut broken = ls.clone();
    for (key, bad) in [
        ("limit", Value::String("100".to_owned())),
        ("total", json!(-1)),
        ("source", Value::String("magic".to_owned())),
        ("dir", Value::String("sideways".to_owned())),
        ("sort", Value::String("whatever".to_owned())),
        ("warnings", Value::String("none".to_owned())),
        ("filters", json!({ "status": ["paused"] })),
        ("lmit", json!(10)),
    ] {
        broken["_meta"] = ls["_meta"].clone();
        broken["_meta"][key] = bad.clone();
        assert!(
            list.validate(&broken).is_err(),
            "item-list.json accepts _meta.{key} = {bad}"
        );
    }

    // The other envelopes describe their own `_meta` too.
    let mut bad_comments = comments.clone();
    bad_comments["_meta"]["skip_newest"] = Value::String("0".to_owned());
    assert!(comments_schema.validate(&bad_comments).is_err());
    let mut bad_stats = stats.clone();
    bad_stats["_meta"]["snapshotted"] = Value::String("false".to_owned());
    assert!(stats_schema.validate(&bad_stats).is_err());

    // An `export json` envelope is a list envelope with a *different* `_meta`
    // (no window, plus the container version), and it has to keep validating —
    // one schema covers every producer of this shape.
    let out = clove(dir.path())
        .args(["export", "json", "--format", "json"])
        .output()
        .unwrap();
    let exported: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        exported["_meta"]["clove_export"]["format"].is_number(),
        "export provenance: {exported}"
    );
    assert_valid(&list, &exported);
}
