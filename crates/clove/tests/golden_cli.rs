//! Golden CLI snapshot tests (M0 acceptance gate: "Golden CLI snapshot tests
//! pass"). They run the real `clove` binary against the committed
//! `tests/fixtures/golden_repo` (2 dependency chains + 1 cycle, all with fixed
//! timestamps) and snapshot the JSON `data` with `insta`.
//!
//! Output is forced onto the file-scan path (`--no-index`) so results never
//! depend on whether an index happens to exist, and item ordering is sorted in
//! the test so snapshots are stable regardless of internal iteration order.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A `clove` invocation rooted at `dir` with a clean, deterministic environment.
fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

/// `clove init` a fresh temp repo, then copy the committed golden fixture's
/// item files into `.clove/issues/`.
fn golden_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();

    let issues = dir.path().join(".clove").join("issues");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden_repo")
        .join("issues");
    for entry in std::fs::read_dir(&fixture).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            std::fs::copy(&path, issues.join(path.file_name().unwrap())).unwrap();
        }
    }
    dir
}

/// Run a `clove` subcommand on the file path as JSON and return the parsed
/// envelope, asserting success.
fn run_json(dir: &Path, args: &[&str]) -> Value {
    let out = clove(dir)
        .arg("--no-index")
        .arg("--format")
        .arg("json")
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "`clove {args:?}` failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(v["ok"], true, "envelope not ok: {v}");
    v
}

/// Collect the `id` strings from a list `data` payload, sorted.
fn sorted_ids(data: &Value) -> Vec<String> {
    let mut ids: Vec<String> = data
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

#[test]
fn golden_ls_lists_all_seven_items() {
    let dir = golden_repo();
    let v = run_json(dir.path(), &["ls"]);

    // Sort the data array by id so the snapshot is order-independent while still
    // capturing every field of every item.
    let mut items = v["data"].as_array().unwrap().clone();
    items.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));

    insta::assert_json_snapshot!("golden_ls", items);
}

#[test]
fn golden_ready_set_is_b_and_e() {
    let dir = golden_repo();
    let v = run_json(dir.path(), &["ready"]);
    // C is closed → B ready; E has no deps → ready. A and D are blocked; F/G are
    // in the cycle and never ready.
    insta::assert_json_snapshot!("golden_ready_ids", sorted_ids(&v["data"]));
}

#[test]
fn golden_blocked_set_is_a_and_d() {
    let dir = golden_repo();
    let v = run_json(dir.path(), &["blocked"]);
    insta::assert_json_snapshot!("golden_blocked_ids", sorted_ids(&v["data"]));
}

#[test]
fn golden_dep_cycle_detects_f_g() {
    let dir = golden_repo();
    let v = run_json(dir.path(), &["dep", "cycle"]);

    // Normalize: sort members within each cycle and sort the list of cycles, so
    // the snapshot does not depend on cycle rotation or discovery order. The
    // `dep cycle` payload is the cycles array directly (`data: [[...]]`).
    let mut cycles: Vec<Vec<String>> = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cycle| {
            let mut members: Vec<String> = cycle
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap().to_owned())
                .collect();
            members.sort();
            members.dedup();
            members
        })
        .collect();
    cycles.sort();
    insta::assert_json_snapshot!("golden_cycles", cycles);
}

#[test]
fn golden_dep_cycle_exit_codes() {
    let dir = golden_repo();
    // Without --fail-on-cycle the command still succeeds (exit 0) while printing
    // the cycle data.
    clove(dir.path())
        .args(["--no-index", "dep", "cycle"])
        .assert()
        .success();
    // With --fail-on-cycle a present cycle yields the dedicated exit code 3.
    clove(dir.path())
        .args(["--no-index", "dep", "cycle", "--fail-on-cycle"])
        .assert()
        .code(3);
}

#[test]
fn golden_dep_tree_of_chain_head() {
    let dir = golden_repo();
    let v = run_json(dir.path(), &["dep", "tree", "proj-AAAAAAAA", "--flat"]);
    // Flat form is a deterministic [{id, depth, ...}] array down chain one.
    insta::assert_json_snapshot!("golden_dep_tree_flat", v["data"]);
}
