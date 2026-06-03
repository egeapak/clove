//! GitHub import/export integration coverage (T-M03).
//!
//! The exhaustive **offline** coverage — `clove-meta` encode/decode round-trip,
//! `GitHubIssue → Item` field mapping over committed JSON fixtures, the
//! idempotency filter, and export-body encoding — lives in the
//! `clove_import::github` unit tests (`crates/clove-import/src/github.rs`),
//! which run with no network and no token. This file holds:
//!
//! 1. the offline CLI-surface checks for `export github --dry-run` (no token), and
//! 2. the **token-gated** network round-trip, which is `#[ignore]`-by-default and
//!    additionally short-circuits when `GITHUB_TOKEN` is unset so CI/sandbox stays
//!    green.

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

#[test]
fn export_github_dry_run_lists_local_items_offline() {
    // Create one local item, then dry-run an export: it must appear in
    // would_create (no external_ref yet) without any network/token.
    let dir = init_repo();
    clove(dir.path())
        .args(["new", "Fix the bug", "--type", "bug"])
        .assert()
        .success();

    let out = clove(dir.path())
        .args([
            "export",
            "github",
            "ege/clove",
            "--dry-run",
            "--format",
            "json",
        ])
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert!(out.status.success(), "dry-run export failed: {out:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    let created = v["data"]["would_create"].as_array().unwrap();
    assert_eq!(created.len(), 1, "one local item would be created: {v}");
    assert_eq!(created[0]["title"], "Fix the bug", "{v}");
}

/// Token-gated network round-trip: export local items to a scratch GitHub repo,
/// then re-import them and assert idempotency.
///
/// This is **ignored by default** (it needs network + write access to a real
/// repo) and additionally returns early if `GITHUB_TOKEN` is unset, so it never
/// fails CI/sandbox. To run it locally:
///
/// ```text
/// GITHUB_TOKEN=ghp_xxx CLOVE_TEST_GH_REPO=youruser/scratch-repo \
///     cargo test -p clove --test import_github -- --ignored github_roundtrip
/// ```
#[test]
#[ignore = "needs GITHUB_TOKEN + a writable scratch repo (CLOVE_TEST_GH_REPO)"]
fn github_roundtrip() {
    let (Ok(_token), Ok(repo)) = (
        std::env::var("GITHUB_TOKEN"),
        std::env::var("CLOVE_TEST_GH_REPO"),
    ) else {
        // No token / repo configured: skip cleanly.
        eprintln!("skipping github_roundtrip: set GITHUB_TOKEN and CLOVE_TEST_GH_REPO");
        return;
    };

    let dir = init_repo();
    clove(dir.path())
        .args(["new", "Roundtrip item", "--type", "bug"])
        .assert()
        .success();

    // Push to GitHub (real network write).
    clove(dir.path())
        .args(["export", "github", &repo, "--format", "json"])
        .assert()
        .success();

    // Import into a fresh repo.
    let dir2 = init_repo();
    clove(dir2.path())
        .args(["import", "github", &repo, "--format", "json"])
        .assert()
        .success();

    // Re-import must be idempotent (everything skipped on the second pass).
    let out = clove(dir2.path())
        .args(["import", "github", &repo, "--dry-run", "--format", "json"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["data"]["would_create"].as_array().unwrap().is_empty(),
        "re-import should create nothing: {v}"
    );
}

/// Without the `github` build feature both `import github` and `export github`
/// must fail with a clean fallback error — not a panic, not an item-not-found,
/// not a silent success. Gated on `not(feature = "github")` so it compiles and
/// runs only under `cargo test -p clove --no-default-features` (the default
/// build has `github` on, where the network paths above apply instead).
#[cfg(not(feature = "github"))]
#[test]
fn github_without_feature_returns_clean_fallback_error() {
    let dir = init_repo();
    clove(dir.path())
        .args(["new", "Local item"])
        .assert()
        .success();

    for args in [
        ["export", "github", "owner/repo"].as_slice(),
        ["import", "github", "owner/repo"].as_slice(),
    ] {
        let out = clove(dir.path())
            .args(args)
            .env_remove("GITHUB_TOKEN")
            .output()
            .unwrap();
        // NotYetImplemented → exit 1 (Usage). Clean error, never a panic/abort.
        assert_eq!(
            out.status.code(),
            Some(1),
            "`{args:?}` without the github feature must be a clean fallback error"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.to_lowercase().contains("github"),
            "fallback error should mention github: {stderr}"
        );
    }
}
