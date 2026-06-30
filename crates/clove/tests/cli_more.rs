//! Additional end-to-end CLI tests for the `clove` binary.
//!
//! These cover command surface and edge cases NOT exercised by
//! `cli_commands.rs`: multi-field `set`/`edit`, `start`/`status` validation,
//! `assign`, `dep tree`/`dep rm`/`dep cycle`, field projection, `jsonl` output,
//! pagination, `query` (flag + stdin), `version`, `agent-doc --format json`,
//! the no-repo error, `--clove-dir`, `comments --limit`, and `--quiet`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A `clove` invocation rooted at `dir`, with a clean environment.
fn clove(dir: &Path) -> Command {
    let mut c = Command::cargo_bin("clove").unwrap();
    c.current_dir(dir);
    c.env_remove("CLOVE_FORMAT");
    c.env_remove("EDITOR");
    c.env("CLOVE_AUTHOR", "tester@example.com");
    c
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

/// Run a command expecting a JSON success envelope and return the parsed value.
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

/// Spawn `cmd` with `input` piped to stdin and capture its output.
fn run_with_stdin(cmd: &mut Command, input: &str) -> std::process::Output {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// Collect the `id` strings from a JSON list envelope's `data` array.
fn ids_of(v: &Value) -> Vec<String> {
    v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
}

// --- set / edit -----------------------------------------------------------

#[test]
fn set_applies_multiple_fields_atomically() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Multi", &[]);

    // One call mutates status, priority, and type together.
    let v =
        json_ok(clove(dir.path()).args(["set", &id, "status=closed", "priority=0", "type=bug"]));
    assert_eq!(v["data"]["status"], "closed");
    assert_eq!(v["data"]["priority"], 0);
    assert_eq!(v["data"]["type"], "bug");
    // Closing via `set` populates the closed timestamp invariant.
    assert!(v["data"]["closed"].is_string());
}

#[test]
fn edit_field_matches_set_semantics() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Edited", &[]);

    let v = json_ok(clove(dir.path()).args([
        "edit",
        &id,
        "--field",
        "status=closed",
        "--field",
        "priority=1",
    ]));
    assert_eq!(v["data"]["status"], "closed");
    assert_eq!(v["data"]["priority"], 1);
    assert!(v["data"]["closed"].is_string());
}

#[test]
fn labels_plus_minus_add_and_remove_canonicalized() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Labelable", &[]);

    // `labels+=` adds (canonicalized to lowercase).
    let added = json_ok(clove(dir.path()).args(["set", &id, "labels+=Area:iOS"]));
    assert_eq!(added["data"]["labels"], serde_json::json!(["area:ios"]));

    // `labels-=` removes regardless of input case.
    let removed = json_ok(clove(dir.path()).args(["set", &id, "labels-=AREA:IOS"]));
    assert_eq!(removed["data"]["labels"], serde_json::json!([]));
}

#[test]
fn set_unknown_field_key_exits_4() {
    let dir = init_repo();
    let id = new_item(dir.path(), "X", &[]);
    clove(dir.path())
        .args(["set", &id, "bogus=1"])
        .assert()
        .failure()
        .code(4);
}

#[test]
fn set_priority_out_of_range_exits_4() {
    let dir = init_repo();
    let id = new_item(dir.path(), "X", &[]);
    clove(dir.path())
        .args(["set", &id, "priority=9"])
        .assert()
        .failure()
        .code(4);
}

// --- start / status -------------------------------------------------------

#[test]
fn start_sets_status_in_progress() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Workable", &[]);
    let v = json_ok(clove(dir.path()).args(["start", &id]));
    assert_eq!(v["data"]["status"], "in_progress");
}

#[test]
fn status_with_invalid_state_exits_4() {
    let dir = init_repo();
    let id = new_item(dir.path(), "X", &[]);
    clove(dir.path())
        .args(["status", &id, "bogus"])
        .assert()
        .failure()
        .code(4);
}

// --- assign ---------------------------------------------------------------

#[test]
fn assign_sets_and_clears_assignee() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Assignable", &[]);

    let assigned = json_ok(clove(dir.path()).args(["assign", &id, "alice"]));
    assert_eq!(assigned["data"]["assignee"], "alice");

    let cleared = json_ok(clove(dir.path()).args(["assign", &id, "--clear"]));
    assert!(cleared["data"]["assignee"].is_null());
}

#[test]
fn assign_without_who_or_clear_exits_4() {
    let dir = init_repo();
    let id = new_item(dir.path(), "X", &[]);
    clove(dir.path())
        .args(["assign", &id])
        .assert()
        .failure()
        .code(4);
}

// --- dep tree / dep rm ----------------------------------------------------

#[test]
fn dep_tree_nests_children_along_dependencies() {
    let dir = init_repo();
    // C is a leaf; B depends on C; A depends on B. tree(A): A -> B -> C.
    let c = new_item(dir.path(), "Ccc", &[]);
    let b = new_item(dir.path(), "Bbb", &["--dep", &c]);
    let a = new_item(dir.path(), "Aaa", &["--dep", &b]);

    let v = json_ok(clove(dir.path()).args(["dep", "tree", &a]));
    let root = &v["data"];
    // Each node carries the documented shape.
    for key in ["id", "title", "status", "ready", "cycle_ref", "children"] {
        assert!(root.get(key).is_some(), "missing {key}: {root}");
    }
    assert_eq!(root["id"], a);
    let b_node = &root["children"][0];
    assert_eq!(b_node["id"], b);
    let c_node = &b_node["children"][0];
    assert_eq!(c_node["id"], c);
    assert_eq!(c_node["children"].as_array().unwrap().len(), 0);
    // Leaf with no deps is ready; nodes above it are not.
    assert_eq!(c_node["ready"], true);
    assert_eq!(root["ready"], false);
}

#[test]
fn dep_tree_flat_emits_array_with_depth() {
    let dir = init_repo();
    let c = new_item(dir.path(), "Ccc", &[]);
    let b = new_item(dir.path(), "Bbb", &["--dep", &c]);
    let a = new_item(dir.path(), "Aaa", &["--dep", &b]);

    let v = json_ok(clove(dir.path()).args(["dep", "tree", &a, "--flat"]));
    let arr = v["data"].as_array().unwrap();
    assert_eq!(arr.len(), 3);
    let depths: Vec<u64> = arr.iter().map(|n| n["depth"].as_u64().unwrap()).collect();
    assert_eq!(depths, vec![0, 1, 2]);
    assert_eq!(arr[0]["id"], a);
}

#[test]
fn dep_tree_depth_limits_nesting() {
    let dir = init_repo();
    let c = new_item(dir.path(), "Ccc", &[]);
    let b = new_item(dir.path(), "Bbb", &["--dep", &c]);
    let a = new_item(dir.path(), "Aaa", &["--dep", &b]);

    // --depth 1 keeps one level of children; the grandchild is pruned.
    let v = json_ok(clove(dir.path()).args(["dep", "tree", &a, "--depth", "1"]));
    let b_node = &v["data"]["children"][0];
    assert_eq!(b_node["id"], b);
    assert_eq!(b_node["children"].as_array().unwrap().len(), 0);
}

#[test]
fn dep_rm_shrinks_deps_array() {
    let dir = init_repo();
    let dep = new_item(dir.path(), "Dep", &[]);
    let item = new_item(dir.path(), "Item", &["--dep", &dep]);

    let before = json_ok(clove(dir.path()).arg("show").arg(&item));
    assert_eq!(before["data"]["deps"], serde_json::json!([dep]));

    let after = json_ok(clove(dir.path()).args(["dep", "rm", &item, &dep]));
    assert_eq!(after["data"]["deps"], serde_json::json!([]));
}

// --- dep cycle ------------------------------------------------------------

#[test]
fn dep_cycle_no_cycle_is_empty_and_exit_0() {
    let dir = init_repo();
    let dep = new_item(dir.path(), "Dep", &[]);
    new_item(dir.path(), "Item", &["--dep", &dep]);

    let v = json_ok(clove(dir.path()).arg("dep").arg("cycle"));
    assert_eq!(v["data"].as_array().unwrap().len(), 0);
    assert_eq!(v["_meta"]["count"], 0);

    // --fail-on-cycle still exits 0 when there is no cycle.
    clove(dir.path())
        .args(["dep", "cycle", "--fail-on-cycle"])
        .assert()
        .success();
}

#[test]
fn dep_cycle_detects_a_hand_written_cycle() {
    let dir = init_repo();
    // The CLI refuses to *form* a cycle (dep add both directions is blocked),
    // so we hand-edit the two item files into a mutual dependency.
    let a = new_item(dir.path(), "Aaa", &[]);
    let b = new_item(dir.path(), "Bbb", &[]);
    for (x, y) in [(&a, &b), (&b, &a)] {
        let path = dir.path().join(format!(".clove/issues/{x}.md"));
        let contents = std::fs::read_to_string(&path).unwrap();
        let contents = contents.replace("deps: []", &format!("deps:\n  - {y}"));
        std::fs::write(&path, contents).unwrap();
    }

    // Plain `dep cycle` lists the cycle but exits 0.
    let v = json_ok(clove(dir.path()).arg("dep").arg("cycle"));
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    assert_eq!(v["_meta"]["count"], 1);

    // `--fail-on-cycle` exits 3 in the presence of a cycle.
    clove(dir.path())
        .args(["dep", "cycle", "--fail-on-cycle"])
        .assert()
        .failure()
        .code(3);
}

// --- field projection -----------------------------------------------------

#[test]
fn ls_fields_projects_only_requested_keys() {
    let dir = init_repo();
    new_item(dir.path(), "One", &[]);
    new_item(dir.path(), "Two", &[]);

    let v = json_ok(clove(dir.path()).args(["ls", "--fields", "id,status"]));
    for obj in v["data"].as_array().unwrap() {
        let keys: Vec<&str> = obj
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys.len(), 2, "extra keys: {keys:?}");
        assert!(keys.contains(&"id"));
        assert!(keys.contains(&"status"));
    }
}

#[test]
fn show_fields_projects_only_requested_keys() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Showable", &[]);
    let v = json_ok(clove(dir.path()).args(["show", &id, "--fields", "id,title"]));
    let keys: Vec<&str> = v["data"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys.len(), 2, "extra keys: {keys:?}");
    assert!(keys.contains(&"id"));
    assert!(keys.contains(&"title"));
}

// --- jsonl ----------------------------------------------------------------

#[test]
fn ls_jsonl_emits_one_envelope_per_item() {
    let dir = init_repo();
    new_item(dir.path(), "One", &[]);
    new_item(dir.path(), "Two", &[]);
    new_item(dir.path(), "Three", &[]);

    let out = clove(dir.path())
        .args(["ls", "--format", "jsonl", "--no-index"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3, "one line per item");
    for line in lines {
        let v: Value = serde_json::from_str(line).expect("each line is valid JSON");
        assert_eq!(v["ok"], true);
        assert!(v["data"].is_object());
        assert!(v["data"]["id"].is_string());
    }
}

// --- pagination -----------------------------------------------------------

#[test]
fn ls_pagination_pages_cover_all_without_overlap() {
    let dir = init_repo();
    let mut all: Vec<String> = Vec::new();
    for i in 0..7 {
        all.push(new_item(dir.path(), &format!("Item {i}"), &[]));
    }

    // --no-index keeps the deterministic file path.
    let page1 = json_ok(clove(dir.path()).args(["ls", "--no-index", "--limit", "3"]));
    assert_eq!(page1["data"].as_array().unwrap().len(), 3);
    assert_eq!(page1["_meta"]["total"], 7);

    let page2 =
        json_ok(clove(dir.path()).args(["ls", "--no-index", "--limit", "3", "--offset", "3"]));
    assert_eq!(page2["data"].as_array().unwrap().len(), 3);

    let page3 =
        json_ok(clove(dir.path()).args(["ls", "--no-index", "--limit", "3", "--offset", "6"]));
    assert_eq!(page3["data"].as_array().unwrap().len(), 1);

    // Pages are disjoint and their union is the full set.
    let mut seen: Vec<String> = Vec::new();
    for page in [&page1, &page2, &page3] {
        seen.extend(ids_of(page));
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 7, "pages overlap or miss items");
    let mut expected = all.clone();
    expected.sort();
    assert_eq!(unique, expected);
}

// --- query ----------------------------------------------------------------

#[test]
fn query_filter_flag_filters_by_status() {
    let dir = init_repo();
    let open = new_item(dir.path(), "Open one", &[]);
    let closing = new_item(dir.path(), "Closed one", &[]);
    clove(dir.path())
        .arg("close")
        .arg(&closing)
        .assert()
        .success();

    let v = json_ok(clove(dir.path()).args(["query", "--filter", r#"{"status":"open"}"#]));
    let ids = ids_of(&v);
    assert!(ids.contains(&open));
    assert!(!ids.contains(&closing));
}

#[test]
fn query_reads_filter_from_stdin() {
    let dir = init_repo();
    let bug = new_item(dir.path(), "A bug", &["--type", "bug"]);
    new_item(dir.path(), "A feature", &["--type", "feature"]);

    let out = run_with_stdin(
        clove(dir.path()).args(["query", "--format", "json"]),
        r#"{"type":"bug"}"#,
    );
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    let ids = ids_of(&v);
    assert_eq!(ids, vec![bug]);
}

#[test]
fn query_empty_stdin_returns_all() {
    let dir = init_repo();
    new_item(dir.path(), "One", &[]);
    new_item(dir.path(), "Two", &[]);

    // Piped-but-empty stdin means "no filter" -> everything matches.
    let out = run_with_stdin(clove(dir.path()).args(["query", "--format", "json"]), "");
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
}

// --- version / agent-doc --------------------------------------------------

#[test]
fn version_json_reports_clove_and_schema() {
    let dir = init_repo();
    let v = json_ok(clove(dir.path()).arg("version"));
    assert_eq!(v["ok"], true);
    assert!(v["data"]["clove"].is_string());
    assert!(
        v["data"]["schema"].is_u64() || v["data"]["schema"].is_i64(),
        "schema must be an integer: {}",
        v["data"]["schema"]
    );
}

#[test]
fn agent_doc_json_has_schema_and_markdown() {
    let dir = init_repo();
    let v = json_ok(clove(dir.path()).arg("agent-doc"));
    assert!(
        v["data"]["schema"].is_u64() || v["data"]["schema"].is_i64(),
        "schema must be an integer"
    );
    let md = v["data"]["markdown"].as_str().expect("markdown string");
    assert!(
        md.contains("generated-by: clove"),
        "markdown missing the generated-by marker"
    );
}

// --- no repo / --clove-dir ------------------------------------------------

#[test]
fn ls_with_no_repo_exits_5_with_no_repo_envelope() {
    // A bare temp dir with no .clove anywhere up the tree.
    let dir = tempfile::tempdir().unwrap();

    // Human mode exits 5.
    clove(dir.path()).arg("ls").assert().failure().code(5);

    // JSON mode yields a NO_REPO error envelope on stdout.
    let out = clove(dir.path())
        .args(["ls", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(5));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "NO_REPO");
    assert_eq!(v["error"]["exit"], 5);
}

#[test]
fn clove_dir_flag_points_at_explicit_clove_dir() {
    // Init under a nested directory, then operate via an absolute --clove-dir.
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("workspace");
    std::fs::create_dir_all(&sub).unwrap();
    clove(&sub)
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();

    let clove_dir = sub.join(".clove");
    let clove_dir_str = clove_dir.to_str().unwrap();

    // Create an item through the explicit --clove-dir, from an unrelated cwd.
    let elsewhere = tempfile::tempdir().unwrap();
    let mut create = clove(elsewhere.path());
    create.args(["--clove-dir", clove_dir_str, "new", "Remote item"]);
    let created = json_ok(&mut create);
    let id = created["data"]["id"].as_str().unwrap().to_owned();

    // And list it back through the same flag.
    let mut listing = clove(elsewhere.path());
    listing.args(["--clove-dir", clove_dir_str, "ls"]);
    let v = json_ok(&mut listing);
    assert!(ids_of(&v).contains(&id));
}

// --- comments --limit -----------------------------------------------------

#[test]
fn comments_limit_returns_most_recent_n() {
    let dir = init_repo();
    let id = new_item(dir.path(), "Discussed", &[]);
    for msg in ["first", "second", "third"] {
        clove(dir.path())
            .args(["comment", &id, msg])
            .assert()
            .success();
    }

    let v = json_ok(clove(dir.path()).args(["comments", &id, "--limit", "2"]));
    let comments = v["data"].as_array().unwrap();
    assert_eq!(comments.len(), 2, "limit caps the count");
    // Comments are returned oldest-first; the most recent two are second/third.
    let bodies: Vec<&str> = comments
        .iter()
        .map(|c| c["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, vec!["second", "third"]);
}

// --- quiet ----------------------------------------------------------------

#[test]
fn quiet_suppresses_human_dependent_warning_on_close() {
    let dir = init_repo();
    // `dep` is depended on by `dependent`; closing `dep` would warn.
    let dep = new_item(dir.path(), "Dep", &[]);
    new_item(dir.path(), "Dependent", &["--dep", &dep]);

    // Without --quiet, human mode emits a warning to stderr.
    let noisy = clove(dir.path()).arg("close").arg(&dep).output().unwrap();
    assert!(noisy.status.success());
    let noisy_err = String::from_utf8_lossy(&noisy.stderr);
    assert!(
        noisy_err.contains("warning"),
        "expected a dependent warning, got: {noisy_err:?}"
    );

    // Reopen, then close again with --quiet: stderr must be silent.
    clove(dir.path())
        .args(["status", &dep, "open"])
        .assert()
        .success();
    let quiet = clove(dir.path())
        .args(["--quiet", "close", &dep])
        .output()
        .unwrap();
    assert!(quiet.status.success());
    let quiet_err = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        quiet_err.trim().is_empty() && !quiet_err.contains("warning"),
        "--quiet must suppress warnings, got: {quiet_err:?}"
    );
}

// --- init argument validation --------------------------------------------

/// `init --prefix <bad>` used to write the invalid prefix to config.toml and
/// succeed, bricking the repo: every later command then failed to load config.
/// It must now be rejected at parse time, before anything is written.
#[test]
fn init_rejects_invalid_prefix_and_writes_nothing() {
    // One representative of each rejection class: uppercase, illegal char,
    // empty, and over-length (max is 8).
    for bad in ["UP", "with-dash", "", "toolongprefix"] {
        let dir = tempfile::tempdir().unwrap();
        let out = clove(dir.path())
            .args(["init", "--prefix", bad])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "init --prefix {bad:?} should fail, but succeeded"
        );
        // Nothing may be left behind: no half-initialized .clove/.
        assert!(
            !dir.path().join(".clove").exists(),
            "init --prefix {bad:?} must not create .clove/ on rejection"
        );
    }
}

/// A valid prefix still initializes, and a follow-up command loads cleanly —
/// guards against the validator over-rejecting (e.g. the 8-char boundary).
#[test]
fn init_accepts_valid_prefix() {
    for good in ["a", "proj", "abcd1234"] {
        let dir = tempfile::tempdir().unwrap();
        clove(dir.path())
            .args(["init", "--prefix", good])
            .assert()
            .success();
        clove(dir.path()).arg("ls").assert().success();
    }
}
