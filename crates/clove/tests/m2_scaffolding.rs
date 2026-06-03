//! M2 CLI-surface tests: the `import`/`export`/`merge-driver` commands parse and
//! reach their handlers. Stub handlers (Phase 5 `import`/`export github`) return
//! a clean "not yet implemented" error; the Phase 2 `merge-driver` is wired and
//! implemented (see `tests/merge_driver.rs` for its full coverage).

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// A `clove` invocation rooted at `dir`, with a clean environment.
fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env_remove("EDITOR");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
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

#[test]
fn import_help_lists_sources() {
    let out = clove(Path::new("."))
        .args(["import", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success(), "import --help failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("tk"), "missing tk source:\n{text}");
    assert!(text.contains("beads"), "missing beads source:\n{text}");
    assert!(text.contains("github"), "missing github source:\n{text}");
}

#[test]
fn export_help_lists_formats() {
    let out = clove(Path::new("."))
        .args(["export", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success(), "export --help failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("json"), "missing json format:\n{text}");
    assert!(text.contains("jsonl"), "missing jsonl format:\n{text}");
    assert!(text.contains("github"), "missing github format:\n{text}");
    assert!(text.contains("--out"), "missing --out flag:\n{text}");
    assert!(
        text.contains("--dry-run"),
        "missing --dry-run flag:\n{text}"
    );
}

#[test]
fn merge_driver_help_lists_positionals() {
    let out = clove(Path::new("."))
        .args(["merge-driver", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success(), "merge-driver --help failed: {out:?}");
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(text.contains("ancestor"), "missing ancestor:\n{text}");
    assert!(text.contains("ours"), "missing ours:\n{text}");
    assert!(text.contains("theirs"), "missing theirs:\n{text}");
    assert!(text.contains("marker"), "missing marker_size:\n{text}");
}

#[test]
fn export_github_dry_run_is_offline_and_ok() {
    // `export github` is implemented (Phase 5). `--dry-run` is fully offline: it
    // partitions local items into would-create / would-update without contacting
    // GitHub, so it succeeds with NO token present (CI/sandbox safe). An empty
    // repo yields an empty plan.
    let dir = init_repo();
    let out = clove(dir.path())
        .args([
            "export",
            "github",
            "ege/clove",
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "export github --dry-run failed: {out:?}"
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON envelope on stdout");
    assert_eq!(v["ok"], true, "dry-run envelope should be ok: {v}");
    assert!(
        v["data"]["would_create"].is_array(),
        "dry-run plan exposes would_create: {v}"
    );
    assert!(
        v["data"]["would_update"].is_array(),
        "dry-run plan exposes would_update: {v}"
    );
}

#[test]
fn export_github_requires_a_target() {
    // Without an `owner/repo` target, `export github` is a validation error
    // (not a parse error), recognized but rejected.
    let dir = init_repo();
    let out = clove(dir.path())
        .args(["export", "github", "--format", "json"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected non-zero exit: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON envelope on stdout");
    assert_eq!(v["ok"], false, "envelope should not be ok: {v}");
}

#[test]
fn import_github_is_recognized_and_needs_a_token() {
    // `import github` is implemented (Phase 5). Without a GITHUB_TOKEN the
    // (non-dry-run) fetch fails cleanly on auth — NOT with NOT_YET_IMPLEMENTED —
    // proving the command is wired through. (Skip the assertion when a token is
    // actually present, e.g. on a developer machine.)
    if std::env::var("GITHUB_TOKEN").is_ok() {
        return;
    }
    let dir = init_repo();
    let out = clove(dir.path())
        .args(["import", "github", "ege/clove", "--format", "json"])
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected non-zero exit: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON envelope on stdout");
    assert_eq!(v["ok"], false, "envelope should not be ok: {v}");
    // It must be an auth/IO failure, not the old NOT_YET_IMPLEMENTED stub.
    assert_ne!(
        v["error"]["code"], "NOT_YET_IMPLEMENTED",
        "github import is implemented now: {v}"
    );
}

#[test]
fn import_beads_is_implemented() {
    // `import beads` is implemented (Phase 4); it is no longer a stub. Full
    // mapping/round-trip coverage lives in `tests/import_beads.rs`; here we only
    // assert the handler is reached and returns a clean success envelope when
    // pointed at a beads `issues.jsonl`.
    let dir = init_repo();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads/issues.jsonl");
    let out = clove(dir.path())
        .args([
            "import",
            "beads",
            fixture.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "import beads failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON envelope on stdout");
    assert_eq!(v["ok"], true, "envelope should be ok: {v}");
}

#[test]
fn merge_driver_is_implemented_and_resolves_identical_sides() {
    // merge-driver is implemented in Phase 2 (no longer a stub). It operates on
    // file paths, not a repository. With identical ours/theirs and no ancestor
    // (add/add), the merge is trivially clean → exit 0 and the result is written
    // to the `%A` (ours) path. Full git-integration coverage lives in
    // `tests/merge_driver.rs`.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let item = "---\nschema: 1\nid: proj-AAAAAAAA\ntitle: x\nstatus: open\ntype: feature\npriority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\nlabels: []\ndeps: []\nrelates: []\nduplicates: []\nsupersedes: []\n---\nbody\n";
    let ours = p.join("ours.md");
    let theirs = p.join("theirs.md");
    std::fs::write(&ours, item).unwrap();
    std::fs::write(&theirs, item).unwrap();

    clove(p)
        .args([
            "merge-driver",
            "/nonexistent-ancestor",
            ours.to_str().unwrap(),
            theirs.to_str().unwrap(),
            "7",
        ])
        .assert()
        .success();
    let merged = std::fs::read_to_string(&ours).unwrap();
    assert!(
        merged.contains("id: proj-AAAAAAAA"),
        "merged item written:\n{merged}"
    );
}
