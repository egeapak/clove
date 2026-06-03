//! T-M02: `clove import beads <issues.jsonl>` end-to-end tests (DESIGN §11.2),
//! plus the T-M04 JSONL round-trip gate.
//!
//! Imports the committed 6-issue fixture (`tests/fixtures/beads/issues.jsonl`)
//! into a fresh `clove init` repo and asserts the field mapping, the `deferred →
//! open + label` and `task → chore` special cases, the typed-dependency split,
//! the `comment_count > 0` stderr warning, the `--dry-run` write-free plan, and
//! idempotent re-import. The round-trip test exports a real repo to JSONL and
//! re-imports it into a fresh repo, then re-imports again to prove idempotency.

use std::path::{Path, PathBuf};
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

/// The committed beads fixture `issues.jsonl`.
fn fixture_jsonl() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads/issues.jsonl")
}

fn init_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", "proj"])
        .assert()
        .success();
    dir
}

/// Count `.md` item files under `.clove/issues/`.
fn item_file_count(dir: &Path) -> usize {
    let issues = dir.join(".clove").join("issues");
    std::fs::read_dir(&issues)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
                .count()
        })
        .unwrap_or(0)
}

/// All items as JSON objects (full `clove ls --format json`).
fn list_items(dir: &Path) -> Vec<Value> {
    let out = clove(dir)
        .args(["ls", "--format", "json", "--limit", "0"])
        .output()
        .unwrap();
    assert!(out.status.success(), "ls failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["data"].as_array().cloned().unwrap_or_default()
}

/// Fetch a single item (full `clove show --format json`) by id.
fn show(dir: &Path, id: &str) -> Value {
    let out = clove(dir)
        .args(["show", id, "--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "show {id} failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["data"].clone()
}

/// Find the imported item whose `external_ref` carries the given beads id.
fn find_by_beads_id<'a>(items: &'a [Value], beads_id: &str) -> &'a Value {
    let key = format!("beads:{beads_id}");
    items
        .iter()
        .find(|i| {
            i["external_ref"]
                .as_str()
                .is_some_and(|r| r == key || r.starts_with(&format!("{key} ")))
        })
        .unwrap_or_else(|| panic!("no item with external_ref for {beads_id} in {items:#?}"))
}

fn str_array(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|x| x.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn maps_all_fields_correctly() {
    let dir = init_repo();
    let out = clove(dir.path())
        .args(["import", "beads", fixture_jsonl().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "import failed: {out:?}");

    // All 6 issues imported as 6 item files.
    assert_eq!(item_file_count(dir.path()), 6);

    let items = list_items(dir.path());
    assert_eq!(items.len(), 6);

    // Every item carries source_system = beads.
    for item in &items {
        assert_eq!(
            item["source_system"], "beads",
            "item missing source_system: {item}"
        );
    }

    // bd-101: feature, in_progress, typed deps split (blocks→deps,
    // related→relates), normalized/deduped labels, unmapped fields in meta blob.
    let f = find_by_beads_id(&items, "bd-101");
    let f = show(dir.path(), f["id"].as_str().unwrap());
    assert_eq!(f["title"], "Article image pipeline");
    assert_eq!(f["type"], "feature");
    assert_eq!(f["status"], "in_progress");
    assert_eq!(f["priority"], 1);
    assert_eq!(f["assignee"], "ege");
    // labels: [Area:Core, perf, perf] → normalized + deduped + sorted.
    assert_eq!(f["labels"], serde_json::json!(["area:core", "perf"]));
    let deps = str_array(&f["deps"]);
    assert!(deps.contains(&"proj-AAAA1111".to_owned()), "deps: {deps:?}");
    assert!(deps.contains(&"proj-BBBB2222".to_owned()), "deps: {deps:?}");
    assert_eq!(f["relates"], serde_json::json!(["proj-CCCC3333"]));
    assert_eq!(f["body"], "Download and compress article images.");
    // Unmapped beads-internal fields preserved in the external_ref meta blob.
    let ext = f["external_ref"].as_str().unwrap();
    assert!(ext.starts_with("beads:bd-101 meta:"), "ext: {ext}");
    assert!(ext.contains("\"epic\":\"e-9\""), "ext: {ext}");
    assert!(ext.contains("\"sprint\":4"), "ext: {ext}");

    // bd-102: issue_type task → chore; owner → assignee.
    let c = find_by_beads_id(&items, "bd-102");
    assert_eq!(c["type"], "chore", "task must map to chore");
    assert_eq!(c["assignee"], "maintainer", "owner must map to assignee");

    // bd-104: parent-child deps → parent (first only); closed status.
    let e = find_by_beads_id(&items, "bd-104");
    assert_eq!(e["type"], "chore");
    assert_eq!(e["parent"], "proj-EEEE5555", "first parent-child wins");
    assert_eq!(e["status"], "closed");
    assert!(e["closed"].is_string(), "closed timestamp set: {e}");

    // bd-105: deferred → open + label `deferred`.
    let d = find_by_beads_id(&items, "bd-105");
    assert_eq!(d["status"], "open", "deferred maps to open");
    let labels = str_array(&d["labels"]);
    assert!(
        labels.contains(&"deferred".to_owned()),
        "deferred label injected: {labels:?}"
    );
    assert!(labels.contains(&"idea".to_owned()), "labels: {labels:?}");

    // bd-106: owner without assignee → owner used.
    let o = find_by_beads_id(&items, "bd-106");
    assert_eq!(o["assignee"], "ops-team");
}

/// Write a `issues.jsonl` file with the given line contents and return both the
/// temp dir (kept alive by the caller) and the file path.
fn write_jsonl(lines: &[&str]) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("issues.jsonl");
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    (dir, path)
}

/// C1: two JSONL lines sharing an id must not collapse onto one staged record.
#[test]
fn duplicate_source_id_is_reported_not_collapsed() {
    let (_src, path) = write_jsonl(&[
        r#"{"id":"bd-dup","title":"First"}"#,
        r#"{"id":"bd-dup","title":"Second"}"#,
    ]);
    let repo = init_repo();

    let out = clove(repo.path())
        .args([
            "import",
            "beads",
            path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "import failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["created"], 1, "only the first dup is written");
    assert_eq!(v["data"]["skipped"], 1, "later dup skipped");
    assert_eq!(item_file_count(repo.path()), 1);

    let items = list_items(repo.path());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "First", "first dup's data preserved");
}

/// M2: one malformed line in the middle does not abort the import — the valid
/// lines still import and the bad line is reported.
#[test]
fn malformed_line_is_skipped_and_reported() {
    let (_src, path) = write_jsonl(&[
        r#"{"id":"bd-1","title":"One"}"#,
        r#"{not valid json"#,
        r#"{"id":"bd-3","title":"Three"}"#,
    ]);
    let repo = init_repo();

    let out = clove(repo.path())
        .args([
            "import",
            "beads",
            path.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "import failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    // Lines 1 and 3 plan to create; line 2 is reported as a skip.
    assert_eq!(v["data"]["would_create"].as_array().unwrap().len(), 2);
    let skips = v["data"]["would_skip"].as_array().unwrap();
    assert!(
        skips.iter().any(|s| s["reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("malformed_line:2"))),
        "expected malformed_line:2 skip, got: {skips:?}"
    );

    // A real import writes both good items.
    clove(repo.path())
        .args(["import", "beads", path.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(item_file_count(repo.path()), 2, "valid lines imported");
}

/// M3: re-importing the same external_ref with a changed status reports the
/// divergence under `conflicts`.
#[test]
fn re_import_with_changed_status_reports_conflict() {
    let (_open_src, open) = write_jsonl(&[r#"{"id":"bd-c","title":"T","status":"open"}"#]);
    let repo = init_repo();
    clove(repo.path())
        .args(["import", "beads", open.to_str().unwrap()])
        .assert()
        .success();

    let (_changed_src, changed) = write_jsonl(&[r#"{"id":"bd-c","title":"T","status":"closed"}"#]);
    let out = clove(repo.path())
        .args([
            "import",
            "beads",
            changed.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "re-import failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let conflicts = v["data"]["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "expected one conflict: {v}");
    assert_eq!(conflicts[0]["field"], "status");
    assert_eq!(conflicts[0]["existing"], "open");
    assert_eq!(conflicts[0]["incoming"], "closed");

    // Unchanged re-import: skipped, no conflicts.
    let out = clove(repo.path())
        .args([
            "import",
            "beads",
            open.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["would_skip"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"]["conflicts"].as_array().unwrap().len(), 0);
}

/// Low: importer warnings (here, comment_count) reach the JSON envelope's
/// `_meta.warnings`, not just stderr.
#[test]
fn comment_count_warning_reaches_json_envelope() {
    let (_src, path) = write_jsonl(&[r#"{"id":"bd-cc","title":"T","comment_count":3}"#]);
    let repo = init_repo();

    let out = clove(repo.path())
        .args([
            "import",
            "beads",
            path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "import failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings = v["_meta"]["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .is_some_and(|s| s.contains("bd-cc") && s.contains("comment"))),
        "comment_count warning missing from _meta.warnings: {warnings:?}"
    );
}

#[test]
fn comment_count_emits_stderr_warning() {
    let dir = init_repo();
    let out = clove(dir.path())
        .args(["import", "beads", fixture_jsonl().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "import failed: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    // bd-103 has comment_count: 2 → warning naming that id and suggesting bd show.
    assert!(
        stderr.contains("bd-103") && stderr.contains("bd show --json bd-103"),
        "expected comment_count warning naming bd-103 on stderr, got:\n{stderr}"
    );
}

#[test]
fn dry_run_writes_zero_files_and_reports_would_create() {
    let dir = init_repo();
    assert_eq!(item_file_count(dir.path()), 0);

    let out = clove(dir.path())
        .args([
            "import",
            "beads",
            fixture_jsonl().to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "dry-run failed: {out:?}");

    // No files written.
    assert_eq!(item_file_count(dir.path()), 0, "dry-run must not write");

    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["would_create"].as_array().unwrap().len(), 6);
    assert_eq!(v["data"]["would_skip"].as_array().unwrap().len(), 0);
    assert_eq!(v["data"]["conflicts"].as_array().unwrap().len(), 0);
}

#[test]
fn re_import_is_idempotent() {
    let dir = init_repo();
    let src = fixture_jsonl();

    // First import writes 6 items.
    clove(dir.path())
        .args(["import", "beads", src.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(item_file_count(dir.path()), 6);

    // Dry-run re-import: every issue matches an existing external_ref → all
    // skipped, zero new files.
    let out = clove(dir.path())
        .args([
            "import",
            "beads",
            src.to_str().unwrap(),
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["would_create"].as_array().unwrap().len(), 0);
    assert_eq!(v["data"]["would_skip"].as_array().unwrap().len(), 6);

    // A real (non-dry-run) re-import also writes nothing new.
    let out = clove(dir.path())
        .args(["import", "beads", src.to_str().unwrap(), "--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["created"], 0);
    assert_eq!(v["data"]["skipped"], 6);
    assert_eq!(item_file_count(dir.path()), 6, "no new files on re-import");
}

/// T-M04: `clove export jsonl` output re-imports losslessly (on mapped fields)
/// and idempotently via `clove import beads`.
#[test]
fn jsonl_round_trip_is_lossless_and_idempotent() {
    // Build a small source repo with deps + labels.
    let src_dir = init_repo();

    let new_id = |args: &[&str]| -> String {
        let out = clove(src_dir.path()).args(args).output().unwrap();
        assert!(out.status.success(), "new failed: {out:?}");
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["data"]["id"].as_str().unwrap().to_owned()
    };

    let a = new_id(&[
        "new",
        "Base task",
        "--type",
        "chore",
        "--priority",
        "3",
        "--label",
        "area:core",
        "--format",
        "json",
    ]);
    let b = new_id(&[
        "new",
        "Depends on base",
        "--type",
        "bug",
        "--priority",
        "1",
        "--dep",
        &a,
        "--label",
        "perf",
        "--format",
        "json",
    ]);

    // Export the source repo to a JSONL file.
    let export_path = src_dir.path().join("out.jsonl");
    clove(src_dir.path())
        .args(["export", "jsonl", "--out", export_path.to_str().unwrap()])
        .assert()
        .success();
    assert!(export_path.exists(), "export did not write file");

    // Import into a FRESH repo.
    let dest = init_repo();
    let out = clove(dest.path())
        .args([
            "import",
            "beads",
            export_path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "round-trip import failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["created"], 2, "two items recreated");
    assert_eq!(item_file_count(dest.path()), 2);

    // Mapped fields intact.
    let items = list_items(dest.path());
    let base = find_by_beads_id(&items, &a);
    assert_eq!(base["title"], "Base task");
    assert_eq!(base["type"], "chore");
    assert_eq!(base["priority"], 3);
    assert_eq!(base["labels"], serde_json::json!(["area:core"]));

    let dep = find_by_beads_id(&items, &b);
    let dep = show(dest.path(), dep["id"].as_str().unwrap());
    assert_eq!(dep["title"], "Depends on base");
    assert_eq!(dep["type"], "bug");
    assert_eq!(dep["priority"], 1);
    assert_eq!(dep["labels"], serde_json::json!(["perf"]));
    // The dep edge survived: it points at the *source* repo's id (preserved as a
    // literal reference through export → import).
    assert_eq!(dep["deps"], serde_json::json!([a]));

    // Re-import into the SAME dest repo: external_ref (the source's
    // `beads:<id>`-derived ref, carried verbatim) matches → zero new items.
    let out = clove(dest.path())
        .args([
            "import",
            "beads",
            export_path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["created"], 0, "re-import creates nothing");
    assert_eq!(v["data"]["skipped"], 2, "re-import skips both");
    assert_eq!(item_file_count(dest.path()), 2, "no new files on re-import");
}
