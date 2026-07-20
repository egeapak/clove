//! `clove sync` is a pure router with **no** built-in providers: every provider
//! resolves to an external `clove-sync-<provider>` plugin (PLUGIN_SYSTEM.md
//! §4.2/§8). A provider that resolves nowhere is a clean, structured validation
//! error (exit 4) — never a panic, a clap parse error, or a silent success.
//!
//! The github happy-path (the whole two-way sync) is covered end-to-end against a
//! mock GitHub server, driving the real `clove-sync-github` plugin, in
//! `tests/sync_github.rs`. This file holds the **miss** case, and deliberately
//! uses `gitlab` (there is no `clove-sync-gitlab`) rather than `github`: a
//! sibling `clove-sync-github` may well be built into `target/debug` and would
//! resolve on the plugin search path, so `github` is not a reliable miss.

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
    // Pin the plugin search path to a directory that holds no plugins, so an
    // unrelated `clove-sync-gitlab` on the real $PATH can never accidentally
    // resolve and make this miss test flaky.
    cmd.env("CLOVE_PLUGIN_PATH", dir);
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
fn sync_unknown_provider_is_a_clean_validation_error() {
    let dir = init_repo();
    clove(dir.path())
        .args(["new", "Local item"])
        .assert()
        .success();

    let out = clove(dir.path())
        // Global flags (--format) must precede the provider — everything after it
        // is captured raw (trailing_var_arg) for plugin forwarding.
        .args(["sync", "--format", "json", "gitlab", "owner/repo"])
        .output()
        .unwrap();

    // A provider miss is a validation error → exit 4 (never a panic/abort/parse
    // error). See `ExitCode`/`error_code` for the VALIDATION_ERROR class.
    assert_eq!(
        out.status.code(),
        Some(4),
        "unknown sync provider must be a validation error: {out:?}"
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid JSON envelope on stdout");
    assert_eq!(v["ok"], false, "envelope should not be ok: {v}");
    assert_eq!(
        v["error"]["code"], "VALIDATION_ERROR",
        "provider miss should classify as VALIDATION_ERROR: {v}"
    );
    let message = v["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("install clove-sync-gitlab"),
        "error should point at the missing plugin: {v}"
    );
}
