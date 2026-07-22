//! End-to-end tests for the built-in `clove import json` / `clove import jsonl`
//! restore (the inverse of `clove export json|jsonl`).
//!
//! `import json`/`jsonl` are **built-in** (in-process), so these run against
//! `cargo_bin("clove")` directly — no plugin build/escargot needed. The core
//! guarantee under test: `export … | import …` into a fresh repo reproduces every
//! item byte-for-byte (ids, status incl. closed, type, priority, deps, parent,
//! labels, body), and re-import is idempotent.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A hermetic `clove` invocation rooted at `dir`.
fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

/// Init a repo with the given id prefix and populate a varied item set:
/// a closed bug, a feature with deps+parent+labels+assignee+body, and an epic.
/// Returns the temp dir and the created ids in creation order.
fn init_populated(prefix: &str) -> (TempDir, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", prefix])
        .assert()
        .success();

    // An epic (parent of the feature) and a bug (a dependency of the feature).
    clove(dir.path())
        .args(["new", "Epic goal", "--type", "epic", "-p", "1"])
        .assert()
        .success();
    clove(dir.path())
        .args(["new", "A bug", "--type", "bug", "-p", "0", "-b", "bug body"])
        .assert()
        .success();

    // Resolve their ids so the feature can be created with the graph edges wired
    // at creation time (`parent`/`deps` are creation-only, not `set`-able tokens).
    let seed = ids_in_order(dir.path());
    let epic = seed
        .iter()
        .find(|id| stored_shape(dir.path(), id)["type"] == "epic")
        .unwrap()
        .clone();
    let bug = seed
        .iter()
        .find(|id| stored_shape(dir.path(), id)["type"] == "bug")
        .unwrap()
        .clone();

    // A feature with labels, assignee, a body, a dep on the bug, and the epic
    // parent — the trickiest shape for the round-trip to preserve.
    clove(dir.path())
        .args([
            "new",
            "A feature",
            "--type",
            "feature",
            "-p",
            "2",
            "-l",
            "area:core",
            "-l",
            "ux",
            "-a",
            "ege",
            "-b",
            "the feature body\nsecond line",
            "--dep",
            &bug,
            "--parent",
            &epic,
        ])
        .assert()
        .success();

    // Close the bug so a closed status + `closed` timestamp is exercised.
    clove(dir.path()).args(["close", &bug]).assert().success();

    let ids = ids_in_order(dir.path());
    (dir, ids)
}

/// Every id in the repo, sorted for a stable comparison.
fn ids_in_order(dir: &Path) -> Vec<String> {
    let out = clove(dir)
        .args(["ls", "--format", "json", "--limit", "0"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut ids: Vec<String> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

/// The full exported shape of one item (`show --format json --verbose`) with the
/// exporter's computed fields stripped, so two repos can be compared for equality
/// of the *stored* fields alone.
fn stored_shape(dir: &Path, id: &str) -> Value {
    let out = clove(dir)
        .args(["show", id, "--format", "json", "--verbose"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut obj = v["data"].as_object().unwrap().clone();
    for computed in [
        "comment_count",
        "ready",
        "blocked_by",
        "dangling_deps",
        "children_summary",
        "warnings",
    ] {
        obj.remove(computed);
    }
    Value::Object(obj)
}

/// Assert repo `b` reproduces every stored field of every item in repo `a`.
fn assert_repos_equal(a: &Path, b: &Path) {
    let ids_a = ids_in_order(a);
    let ids_b = ids_in_order(b);
    assert_eq!(ids_a, ids_b, "same id set after restore");
    for id in &ids_a {
        assert_eq!(
            stored_shape(a, id),
            stored_shape(b, id),
            "item {id} restored verbatim"
        );
    }
}

/// Export repo `src` to `path` via the given format provider (`json`/`jsonl`).
fn export_to(src: &Path, provider: &str, path: &Path) {
    clove(src)
        .args(["export", provider, "--out"])
        .arg(path)
        .assert()
        .success();
}

#[test]
fn json_roundtrip_reproduces_every_item() {
    let (a, _ids) = init_populated("demoa");
    let dump = a.path().join("a.json");
    export_to(a.path(), "json", &dump);

    // A fresh, independent repo (different prefix) — restore must preserve the
    // source ids, not mint new ones under demob.
    let b = tempfile::tempdir().unwrap();
    clove(b.path())
        .args(["init", "--prefix", "demob"])
        .assert()
        .success();

    let assert = clove(b.path())
        .args(["--format", "json", "import", "json"])
        .arg(&dump)
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["created"], 3);
    assert_eq!(v["data"]["skipped"], 0);
    assert_eq!(v["data"]["overwritten"], 0);

    assert_repos_equal(a.path(), b.path());

    // Spot-check the tricky closed bug: status + closed timestamp survived.
    let ids = ids_in_order(a.path());
    let bug = ids
        .iter()
        .find(|id| stored_shape(a.path(), id)["type"] == "bug")
        .unwrap();
    let restored = stored_shape(b.path(), bug);
    assert_eq!(restored["status"], "closed");
    assert!(restored["closed"].is_string(), "closed ts preserved");

    // And the feature's deps/parent/labels/assignee/body.
    let feature = ids
        .iter()
        .find(|id| stored_shape(a.path(), id)["type"] == "feature")
        .unwrap();
    let f = stored_shape(b.path(), feature);
    assert_eq!(f["deps"].as_array().unwrap().len(), 1);
    assert!(f["parent"].is_string());
    assert_eq!(f["labels"], serde_json::json!(["area:core", "ux"]));
    assert_eq!(f["assignee"], "ege");
    assert!(f["body"].as_str().unwrap().contains("the feature body"));
}

#[test]
fn jsonl_roundtrip_reproduces_every_item() {
    let (a, _ids) = init_populated("demoa");
    let dump = a.path().join("a.jsonl");
    export_to(a.path(), "jsonl", &dump);

    let b = tempfile::tempdir().unwrap();
    clove(b.path())
        .args(["init", "--prefix", "demob"])
        .assert()
        .success();

    let assert = clove(b.path())
        .args(["--format", "json", "import", "jsonl"])
        .arg(&dump)
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["created"], 3);

    assert_repos_equal(a.path(), b.path());
}

#[test]
fn reimport_is_idempotent() {
    let (a, _ids) = init_populated("demoa");
    let dump = a.path().join("a.json");
    export_to(a.path(), "json", &dump);

    let b = tempfile::tempdir().unwrap();
    clove(b.path())
        .args(["init", "--prefix", "demob"])
        .assert()
        .success();
    clove(b.path())
        .args(["import", "json"])
        .arg(&dump)
        .assert()
        .success();

    // A second import writes nothing: all ids already exist and are skipped.
    let assert = clove(b.path())
        .args(["--format", "json", "import", "json"])
        .arg(&dump)
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["created"], 0);
    assert_eq!(v["data"]["skipped"], 3);
    assert_eq!(v["data"]["overwritten"], 0);
}

#[test]
fn overwrite_restores_a_locally_changed_item() {
    let (a, _ids) = init_populated("demoa");
    let dump = a.path().join("a.json");
    export_to(a.path(), "json", &dump);

    let b = tempfile::tempdir().unwrap();
    clove(b.path())
        .args(["init", "--prefix", "demob"])
        .assert()
        .success();
    clove(b.path())
        .args(["import", "json"])
        .arg(&dump)
        .assert()
        .success();

    // Mutate one item in B, diverging from A's export.
    let ids = ids_in_order(a.path());
    let feature = ids
        .iter()
        .find(|id| stored_shape(a.path(), id)["type"] == "feature")
        .unwrap();
    clove(b.path())
        .args(["set", feature, "priority=4", "title=Locally changed"])
        .assert()
        .success();
    assert_ne!(
        stored_shape(a.path(), feature),
        stored_shape(b.path(), feature)
    );

    // Without --overwrite the divergent item is skipped (unchanged).
    let assert = clove(b.path())
        .args(["--format", "json", "import", "json"])
        .arg(&dump)
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["skipped"], 3);
    assert_eq!(stored_shape(b.path(), feature)["title"], "Locally changed");

    // With --overwrite the item is restored to A's state.
    let assert = clove(b.path())
        .args(["--format", "json", "import", "json", "--overwrite"])
        .arg(&dump)
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["data"]["overwritten"], 3);
    assert_eq!(v["data"]["created"], 0);
    assert_repos_equal(a.path(), b.path());
}

#[test]
fn dry_run_emits_plan_and_writes_nothing() {
    let (a, _ids) = init_populated("demoa");
    let dump = a.path().join("a.json");
    export_to(a.path(), "json", &dump);

    let b = tempfile::tempdir().unwrap();
    clove(b.path())
        .args(["init", "--prefix", "demob"])
        .assert()
        .success();

    let assert = clove(b.path())
        .args(["--format", "json", "import", "json", "--dry-run"])
        .arg(&dump)
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["would_create"].as_array().unwrap().len(), 3);
    assert!(v["data"]["would_skip"].as_array().unwrap().is_empty());
    assert!(v["data"]["would_overwrite"].as_array().unwrap().is_empty());

    // Nothing was written: B is still empty.
    assert!(ids_in_order(b.path()).is_empty(), "dry run wrote nothing");
}

#[test]
fn newer_container_format_is_rejected_with_exit_4() {
    let b = tempfile::tempdir().unwrap();
    clove(b.path())
        .args(["init", "--prefix", "demob"])
        .assert()
        .success();

    // A hand-crafted export whose container format is from the future.
    let doc = serde_json::json!({
        "v": 1,
        "ok": true,
        "data": [],
        "_meta": { "clove_export": { "format": 999, "item_schema": 1 } },
    });
    let dump = b.path().join("future.json");
    std::fs::write(&dump, serde_json::to_string(&doc).unwrap()).unwrap();

    let assert = clove(b.path())
        .args(["--format", "json", "import", "json"])
        .arg(&dump)
        .assert()
        .failure()
        .code(4);
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["exit"], 4);
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("newer clove") && msg.contains("upgrade"),
        "message: {msg}"
    );
}

#[test]
fn missing_source_file_is_io_error_exit_5() {
    let b = tempfile::tempdir().unwrap();
    clove(b.path())
        .args(["init", "--prefix", "demob"])
        .assert()
        .success();

    clove(b.path())
        .args(["import", "json", "does-not-exist.json"])
        .assert()
        .failure()
        .code(5);
}
