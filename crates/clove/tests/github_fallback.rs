//! `sync github` without the `github` build feature must fail with a clean
//! fallback error — not a panic, not a parse error, not a silent success.
//!
//! The feature-on path (the whole two-way sync) is covered end-to-end against a
//! mock GitHub server in `tests/sync_github.rs`. This file holds only the
//! `not(feature = "github")` fallback, so it compiles and runs solely under
//! `cargo test -p clove --no-default-features`.

#![cfg(not(feature = "github"))]

use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
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

#[test]
fn sync_github_without_feature_returns_clean_fallback_error() {
    let dir = init_repo();
    clove(dir.path())
        .args(["new", "Local item"])
        .assert()
        .success();

    let out = clove(dir.path())
        .args(["sync", "github", "owner/repo"])
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    // NotYetImplemented → exit 1 (Usage). Clean error, never a panic/abort.
    assert_eq!(
        out.status.code(),
        Some(1),
        "`sync github` without the github feature must be a clean fallback error"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("github"),
        "fallback error should mention github: {stderr}"
    );
}
