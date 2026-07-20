//! T-M01: `clove import tk <.tickets dir>` end-to-end tests (DESIGN §11.1).
//!
//! Imports the committed 5-ticket fixture (`tests/fixtures/tk/.tickets/`) into a
//! fresh `clove init` repo and asserts the field mapping, the `--dry-run`
//! write-free plan, the filename-fallback warning, and idempotent re-import.

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

/// The committed tk fixture `.tickets/` directory.
fn fixture_tickets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tk/.tickets")
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

/// Find the imported item whose `external_ref` carries the given tk id.
fn find_by_tk_id<'a>(items: &'a [Value], tk_id: &str) -> &'a Value {
    let key = format!("tk:{tk_id}");
    items
        .iter()
        .find(|i| {
            i["external_ref"]
                .as_str()
                .is_some_and(|r| r == key || r.starts_with(&format!("{key} ")))
        })
        .unwrap_or_else(|| panic!("no item with external_ref for {tk_id} in {items:#?}"))
}

#[test]
fn maps_all_fields_correctly() {
    let dir = init_repo();
    let out = clove(dir.path())
        .args(["import", "tk", fixture_tickets().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "import failed: {out:?}");

    // All 5 tickets imported as 5 item files.
    assert_eq!(item_file_count(dir.path()), 5);

    let items = list_items(dir.path());
    assert_eq!(items.len(), 5);

    // Every item carries source_system = tk.
    for item in &items {
        assert_eq!(
            item["source_system"], "tk",
            "item missing source_system: {item}"
        );
    }

    // tk-101: feature with deps + relates(from links) + normalized/deduped labels +
    // an H1 title stripped from the body.
    let f = find_by_tk_id(&items, "tk-101");
    let f = show(dir.path(), f["id"].as_str().unwrap());
    assert_eq!(f["title"], "Article image download and compression");
    assert_eq!(f["type"], "feature");
    assert_eq!(f["status"], "in_progress");
    assert_eq!(f["priority"], 1);
    // tags: [Area:Core, perf, perf] → normalized + deduped + sorted.
    assert_eq!(f["labels"], serde_json::json!(["area:core", "perf"]));
    let deps: Vec<String> = f["deps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert!(deps.contains(&"proj-3K2MZABC".to_owned()), "deps: {deps:?}");
    assert!(deps.contains(&"proj-9P1QRABC".to_owned()), "deps: {deps:?}");
    assert_eq!(f["relates"], serde_json::json!(["proj-AAAA1111"]));
    // The H1 line is stripped from the stored body.
    let body = f["body"].as_str().unwrap();
    assert!(
        !body.contains("# Article image"),
        "H1 not stripped: {body:?}"
    );
    assert!(
        body.contains("Save compressed versions"),
        "body lost: {body:?}"
    );

    // tk-102: `type: task` → chore.
    let c = find_by_tk_id(&items, "tk-102");
    assert_eq!(c["type"], "chore", "task must map to chore");

    // tk-103: assignee + high priority + an upstream external-ref folded in.
    let b = find_by_tk_id(&items, "tk-103");
    assert_eq!(b["type"], "bug");
    assert_eq!(b["priority"], 0);
    assert_eq!(b["assignee"], "ege");
    assert_eq!(b["external_ref"], "tk:tk-103 upstream:JIRA-4821");

    // tk-104: no H1 → filename stem used as title.
    let n = find_by_tk_id(&items, "tk-104");
    assert_eq!(n["title"], "no-heading-ticket");

    // tk-105: parent reference + closed status.
    let e = find_by_tk_id(&items, "tk-105");
    assert_eq!(e["parent"], "proj-EPIC0001");
    assert_eq!(e["status"], "closed");
    assert!(e["closed"].is_string(), "closed timestamp set: {e}");
}

#[test]
fn missing_h1_emits_filename_fallback_warning() {
    let dir = init_repo();
    let out = clove(dir.path())
        .args(["import", "tk", fixture_tickets().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "import failed: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no-heading-ticket") && stderr.to_lowercase().contains("filename"),
        "expected filename-fallback warning on stderr, got:\n{stderr}"
    );
}

#[test]
fn dry_run_writes_zero_files_and_reports_would_create() {
    let dir = init_repo();
    let before = item_file_count(dir.path());
    assert_eq!(before, 0);

    let out = clove(dir.path())
        .args([
            "import",
            "--format",
            "json",
            "tk",
            fixture_tickets().to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "dry-run failed: {out:?}");

    // No files written.
    assert_eq!(item_file_count(dir.path()), 0, "dry-run must not write");

    // would_create reports all 5 tickets; nothing skipped (empty repo).
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["would_create"].as_array().unwrap().len(), 5);
    assert_eq!(v["data"]["would_skip"].as_array().unwrap().len(), 0);
    assert_eq!(v["data"]["conflicts"].as_array().unwrap().len(), 0);
}

/// Write a `.tickets/` dir with the given `(filename, contents)` pairs and return
/// the temp dir holding it (kept alive by the caller).
fn write_tickets(files: &[(&str, &str)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let tickets = dir.path().join(".tickets");
    std::fs::create_dir_all(&tickets).unwrap();
    for (name, contents) in files {
        std::fs::write(tickets.join(name), contents).unwrap();
    }
    dir
}

/// C1: two tickets sharing `id: tk-dup` with different titles must not collapse
/// onto a single staged record. The duplicate is reported, distinct data kept.
#[test]
fn duplicate_source_id_is_reported_not_collapsed() {
    let src = write_tickets(&[
        ("a.md", "---\nid: tk-dup\n---\n# First Title\n\nBody A.\n"),
        ("b.md", "---\nid: tk-dup\n---\n# Second Title\n\nBody B.\n"),
    ]);
    let tickets = src.path().join(".tickets");
    let repo = init_repo();

    let out = clove(repo.path())
        .args([
            "import",
            "--format",
            "json",
            "tk",
            tickets.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "import failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    // Exactly one created (the first), one skipped as a duplicate — never two
    // identical files for the same source id.
    assert_eq!(v["data"]["created"], 1, "only the first dup is written");
    assert_eq!(v["data"]["skipped"], 1, "the later dup is skipped");
    assert_eq!(item_file_count(repo.path()), 1);

    // The single written item preserves the FIRST ticket's distinct data.
    let items = list_items(repo.path());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "First Title");
}

/// H1: a BOM-prefixed tk ticket imports with its real id/status/frontmatter
/// intact (the BOM must not push the whole file into the body).
#[test]
fn bom_prefixed_ticket_parses_frontmatter() {
    let src = write_tickets(&[(
        "bom.md",
        "\u{FEFF}---\nid: tk-bom\nstatus: closed\ntype: bug\n---\n# BOM Title\n\nBody.\n",
    )]);
    let tickets = src.path().join(".tickets");
    let repo = init_repo();

    clove(repo.path())
        .args(["import", "tk", tickets.to_str().unwrap()])
        .assert()
        .success();

    let items = list_items(repo.path());
    let it = find_by_tk_id(&items, "tk-bom");
    assert_eq!(it["status"], "closed", "BOM dropped frontmatter: {it}");
    assert_eq!(it["type"], "bug");
    assert_eq!(it["title"], "BOM Title");
}

/// M5: a tk ticket whose frontmatter contains a YAML alias is rejected with a
/// clean error, not parsed.
#[test]
fn yaml_alias_in_frontmatter_is_rejected() {
    let src = write_tickets(&[(
        "alias.md",
        "---\nid: tk-alias\nstatus: &a open\ndupe: *a\n---\n# T\n",
    )]);
    let tickets = src.path().join(".tickets");
    let repo = init_repo();

    let out = clove(repo.path())
        .args(["import", "tk", tickets.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "alias ticket must be rejected");
    assert_eq!(item_file_count(repo.path()), 0, "nothing written");
}

/// M5: an oversized tk file (beyond the per-file import ceiling) is rejected
/// cleanly rather than slurped whole and parsed.
#[test]
fn oversized_file_is_rejected() {
    // Ceiling = MAX_FRONTMATTER_BYTES (64KiB) + MAX_BODY_BYTES (4MiB) + 4096.
    let ceiling = 65_536 + 4_194_304 + 4096;
    let mut contents = String::from("---\nid: tk-big\n---\n# Big\n\n");
    contents.push_str(&"x".repeat(ceiling + 1));
    let src = write_tickets(&[("big.md", &contents)]);
    let tickets = src.path().join(".tickets");
    let repo = init_repo();

    let out = clove(repo.path())
        .args(["import", "tk", tickets.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "oversized file must be rejected");
    assert_eq!(item_file_count(repo.path()), 0, "nothing written");
}

/// M4: a dep on an absent id surfaces a dangling-target warning (report-only:
/// the item is still planned/created).
#[test]
fn dangling_dep_emits_warning() {
    let src = write_tickets(&[(
        "dang.md",
        "---\nid: tk-dang\ndeps: [proj-ZZZZ9999]\n---\n# Has Dangling Dep\n",
    )]);
    let tickets = src.path().join(".tickets");
    let repo = init_repo();

    let out = clove(repo.path())
        .args([
            "import",
            "--format",
            "json",
            "tk",
            tickets.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "import failed: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("dangling") && stderr.contains("proj-ZZZZ9999"),
        "expected dangling warning on stderr, got:\n{stderr}"
    );
    // And the warning reaches the JSON envelope's _meta.warnings.
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings = v["_meta"]["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|s| s.contains("proj-ZZZZ9999"))),
        "dangling warning missing from _meta.warnings: {warnings:?}"
    );
}

/// M3: re-importing the same external_ref with a changed status reports the
/// divergence under `conflicts`; an unchanged re-import reports none.
#[test]
fn re_import_with_changed_status_reports_conflict() {
    let open = write_tickets(&[("c.md", "---\nid: tk-c\nstatus: open\n---\n# Same Title\n")]);
    let repo = init_repo();
    clove(repo.path())
        .args([
            "import",
            "tk",
            open.path().join(".tickets").to_str().unwrap(),
        ])
        .assert()
        .success();

    // Re-import the SAME id with a divergent status → conflict on `status`.
    let changed = write_tickets(&[("c.md", "---\nid: tk-c\nstatus: closed\n---\n# Same Title\n")]);
    let out = clove(repo.path())
        .args([
            "import",
            "--format",
            "json",
            "tk",
            changed.path().join(".tickets").to_str().unwrap(),
            "--dry-run",
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

    // An UNCHANGED re-import: skipped with empty conflicts.
    let out = clove(repo.path())
        .args([
            "import",
            "--format",
            "json",
            "tk",
            open.path().join(".tickets").to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["would_skip"].as_array().unwrap().len(), 1);
    assert_eq!(v["data"]["conflicts"].as_array().unwrap().len(), 0);
}

#[test]
fn re_import_is_idempotent() {
    let dir = init_repo();
    let src = fixture_tickets();

    // First import writes 5 items.
    clove(dir.path())
        .args(["import", "tk", src.to_str().unwrap()])
        .assert()
        .success();
    assert_eq!(item_file_count(dir.path()), 5);

    // Second import: every ticket matches an existing external_ref → all skipped,
    // zero new files.
    let out = clove(dir.path())
        .args([
            "import",
            "--format",
            "json",
            "tk",
            src.to_str().unwrap(),
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["would_create"].as_array().unwrap().len(), 0);
    assert_eq!(v["data"]["would_skip"].as_array().unwrap().len(), 5);

    // A real (non-dry-run) re-import also writes nothing new.
    let out = clove(dir.path())
        .args(["import", "--format", "json", "tk", src.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["created"], 0);
    assert_eq!(v["data"]["skipped"], 5);
    assert_eq!(item_file_count(dir.path()), 5, "no new files on re-import");
}
