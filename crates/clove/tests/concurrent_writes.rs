//! Concurrency regression tests for the CLI mutation commands.
//!
//! `clove status`/`set`/`edit --field` once did a lock-free `store.get` followed
//! by a locking `store.update`, leaving a window in which a concurrent writer
//! (the web UI, an MCP agent, the daemon) could commit between the read and the
//! write and have its update silently clobbered. They now go through
//! `ItemStore::update_with`, which holds the store-wide advisory lock across the
//! whole read-modify-write (DESIGN §4).
//!
//! These drive the real binary from several *processes* at once, because that is
//! the scenario the advisory lock exists for — `clove-core`'s own
//! `concurrent_dep_adds_do_not_lose_updates` already covers the in-process case.

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
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

fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    dir
}

fn json_ok(cmd: &mut Command) -> Value {
    let out = cmd.arg("--format").arg("json").output().unwrap();
    assert!(out.status.success(), "command failed: {out:?}");
    serde_json::from_slice(&out.stdout).expect("valid JSON")
}

fn new_item(dir: &Path, title: &str) -> String {
    let v = json_ok(clove(dir).arg("new").arg(title));
    v["data"]["id"].as_str().unwrap().to_owned()
}

fn show(dir: &Path, id: &str) -> Value {
    json_ok(clove(dir).args(["show", id]))["data"].clone()
}

/// Run `count` `clove` invocations concurrently and assert every one succeeded.
fn run_concurrently(dir: &Path, args_per_child: Vec<Vec<String>>) {
    let children: Vec<_> = args_per_child
        .into_iter()
        .map(|args| clove(dir).args(&args).spawn().expect("spawn clove"))
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "concurrent invocation failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn concurrent_set_label_adds_do_not_lose_updates() {
    // Each writer appends a distinct label. Every append is a read-modify-write
    // of the same file, so without a lock held across the whole window the later
    // writers overwrite the earlier ones and labels silently vanish.
    let dir = init_repo();
    let id = new_item(dir.path(), "contended");

    const WRITERS: usize = 8;
    let args: Vec<Vec<String>> = (0..WRITERS)
        .map(|i| {
            vec![
                "set".to_owned(),
                id.clone(),
                format!("labels+=tag-{i}"),
                "--format".to_owned(),
                "json".to_owned(),
            ]
        })
        .collect();
    run_concurrently(dir.path(), args);

    let labels: Vec<String> = show(dir.path(), &id)["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    for i in 0..WRITERS {
        let want = format!("tag-{i}");
        assert!(
            labels.contains(&want),
            "label {want} was lost by a concurrent `clove set` (got {labels:?})"
        );
    }
}

#[test]
fn concurrent_edit_field_adds_do_not_lose_updates() {
    // Same contention through `clove edit --field`, which shares the write path
    // with `clove set`.
    let dir = init_repo();
    let id = new_item(dir.path(), "contended");

    const WRITERS: usize = 8;
    let args: Vec<Vec<String>> = (0..WRITERS)
        .map(|i| {
            vec![
                "edit".to_owned(),
                id.clone(),
                "--field".to_owned(),
                format!("labels+=edit-{i}"),
                "--format".to_owned(),
                "json".to_owned(),
            ]
        })
        .collect();
    run_concurrently(dir.path(), args);

    let labels: Vec<String> = show(dir.path(), &id)["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    for i in 0..WRITERS {
        let want = format!("edit-{i}");
        assert!(
            labels.contains(&want),
            "label {want} was lost by a concurrent `clove edit --field` (got {labels:?})"
        );
    }
}

#[test]
fn concurrent_status_and_label_writes_do_not_clobber_each_other() {
    // A cross-command race: `clove close` and `clove set` contend on one item.
    // The status transition and the label append touch different fields, so a
    // lost update shows up as one of the two edits silently disappearing.
    let dir = init_repo();
    let id = new_item(dir.path(), "contended");

    let args = vec![
        vec![
            "close".to_owned(),
            id.clone(),
            "--format".to_owned(),
            "json".to_owned(),
        ],
        vec![
            "set".to_owned(),
            id.clone(),
            "labels+=survivor".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ],
    ];
    run_concurrently(dir.path(), args);

    let item = show(dir.path(), &id);
    assert_eq!(item["status"], "closed", "the status transition was lost");
    let labels: Vec<String> = item["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert!(
        labels.contains(&"survivor".to_owned()),
        "the label append was lost (got {labels:?})"
    );
    // The closed-timestamp invariant must survive the interleaving too.
    assert!(
        item["closed"].is_string(),
        "closed timestamp missing after a contended close"
    );
}
