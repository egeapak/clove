//! M2 Phase 0 scaffolding tests: the `import`/`export`/`merge-driver` command
//! surface parses and reaches its (stub) handler, which returns a clean
//! "not yet implemented" error rather than panicking. No real interop behavior
//! exists yet (later phases); these only pin the CLI wiring.

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

/// A stub command under `--format json` must emit a valid error envelope with a
/// non-zero exit and the `NOT_YET_IMPLEMENTED` code.
fn assert_not_yet_implemented(cmd: &mut Command) {
    let out = cmd.arg("--format").arg("json").output().unwrap();
    assert!(!out.status.success(), "expected non-zero exit: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON envelope on stdout");
    assert_eq!(v["ok"], false, "envelope should not be ok: {v}");
    assert_eq!(v["error"]["code"], "NOT_YET_IMPLEMENTED", "wrong code: {v}");
    // Usage-class exit code (1).
    assert_eq!(v["error"]["exit"], 1, "wrong exit code: {v}");
}

#[test]
fn export_github_stub_returns_clean_error() {
    // `export json|jsonl` are implemented (Phase 1); `export github` is Phase 5
    // and still returns a clean NOT_YET_IMPLEMENTED error.
    let dir = init_repo();
    assert_not_yet_implemented(clove(dir.path()).args(["export", "github"]));
}

#[test]
fn import_beads_stub_returns_clean_error() {
    let dir = init_repo();
    assert_not_yet_implemented(clove(dir.path()).args(["import", "beads", "issues.jsonl"]));
}

#[test]
fn merge_driver_stub_returns_clean_error() {
    // merge-driver does not require a repository; it operates on file paths.
    let dir = tempfile::tempdir().unwrap();
    assert_not_yet_implemented(clove(dir.path()).args(["merge-driver", "o", "a", "b", "7"]));
}
