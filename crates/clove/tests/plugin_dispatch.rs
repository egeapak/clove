//! End-to-end tests for the plugin dispatch seam (PLUGIN_SYSTEM.md §4–§7).
//!
//! Both tests build the `clove-echo` fixture plugin with escargot (mirroring
//! `mcp.rs`'s `cloved` build), drop it into a temp dir, and point
//! `CLOVE_PLUGIN_PATH` at that dir. `external_plugin_is_dispatched_*` proves the
//! host resolves `clove-echo`, forwards argv, and exports the `CLOVE_*` env the
//! plugin reflects back; `plugin_list_reports_the_plugin` proves `clove plugin
//! list` enumerates it.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// Build the `clove-echo` fixture and copy it into a fresh temp dir as
/// `clove-echo`, returning `(dir, path)`. The temp dir is the value handed to
/// `CLOVE_PLUGIN_PATH` so the search path is deterministic.
fn install_echo() -> (TempDir, PathBuf) {
    let built = escargot::CargoBuild::new()
        .package("clove-plugin-echo")
        .bin("clove-echo")
        .run()
        .expect("build clove-echo fixture");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("clove-echo");
    std::fs::copy(built.path(), &dest).expect("copy clove-echo into the plugin dir");
    (dir, dest)
}

/// A `clove` invocation rooted at `dir` with a hermetic environment.
fn clove(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("clove").unwrap();
    cmd.current_dir(dir);
    cmd.env_remove("CLOVE_FORMAT");
    cmd.env("CLOVE_AUTHOR", "tester@example.com");
    cmd
}

/// A `.clove/` repository with a known id prefix.
fn init_repo(prefix: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    clove(dir.path())
        .args(["init", "--prefix", prefix])
        .assert()
        .success();
    dir
}

#[test]
fn external_plugin_is_dispatched_with_argv_and_env() {
    let (plugin_dir, _echo) = install_echo();
    let repo = init_repo("proj");

    // `--format json` goes BEFORE the external subcommand so the host parses it
    // (a global flag after the plugin name would be forwarded to the plugin).
    let assert = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "json", "echo", "hello", "world"])
        .assert()
        .success();

    let out = assert.get_output();
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {:?}", out));

    // Standard success envelope from the plugin's `clove-plugin` harness.
    assert_eq!(v["ok"], true, "envelope: {v}");

    // The forwarded argv (leading `echo` echo stripped by the plugin harness).
    let argv = v["data"]["argv"].as_array().expect("data.argv array");
    assert!(argv.iter().any(|a| a == "hello"), "argv: {argv:?}");
    assert!(argv.iter().any(|a| a == "world"), "argv: {argv:?}");

    // A generic plugin has no provider; the command is the bare subcommand name;
    // the id prefix comes from the host's resolved config.
    assert!(
        v["data"]["provider"].is_null(),
        "provider should be null: {v}"
    );
    assert_eq!(v["data"]["command"], "echo");
    assert_eq!(v["data"]["id_prefix"], "proj");
    // The exported `CLOVE_DIR` points inside the repo (proves discovery ran).
    let clove_dir = v["data"]["clove_dir"].as_str().unwrap();
    assert!(clove_dir.ends_with(".clove"), "clove_dir: {clove_dir}");
}

/// The `--clove-dir` override must propagate to `CLOVE_DIR` / `CLOVE_SYNC_DIR` /
/// `CLOVE_CONFIG_PATH` (they come from the resolved clove dir, not
/// `root.join(".clove")`). Uses a differently-named symlink to the real `.clove`
/// so the resolved dir and `root/.clove` genuinely differ — a regression that
/// re-derived the dir from `root` would echo `.clove`, not `link-clove`.
#[cfg(unix)]
#[test]
fn clove_dir_override_propagates_to_exported_env() {
    let (plugin_dir, _echo) = install_echo();
    let repo = init_repo("proj");
    let link = repo.path().join("link-clove");
    std::os::unix::fs::symlink(repo.path().join(".clove"), &link).unwrap();

    let assert = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args([
            "--clove-dir",
            link.to_str().unwrap(),
            "--format",
            "json",
            "echo",
        ])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true, "envelope: {v}");

    let clove_dir = v["data"]["clove_dir"].as_str().unwrap();
    let sync_dir = v["data"]["sync_dir"].as_str().unwrap();
    let config_path = v["data"]["config_path"].as_str().unwrap();
    assert!(clove_dir.ends_with("link-clove"), "clove_dir: {clove_dir}");
    assert!(
        sync_dir.ends_with("link-clove/sync"),
        "sync_dir: {sync_dir}"
    );
    assert!(
        config_path.ends_with("link-clove/config.toml"),
        "config_path: {config_path}"
    );
}

#[test]
fn unknown_subcommand_is_a_usage_error() {
    // No CLOVE_PLUGIN_PATH → `clove-nope` resolves nowhere → exit 1 (usage).
    let repo = init_repo("proj");
    let assert = clove(repo.path())
        .env_remove("CLOVE_PLUGIN_PATH")
        .args(["--format", "json", "nope-not-a-command"])
        .assert()
        .failure()
        .code(1);
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "UNKNOWN_SUBCOMMAND");
    assert_eq!(v["error"]["exit"], 1);
}

#[test]
fn plugin_list_reports_the_plugin() {
    let (plugin_dir, _echo) = install_echo();

    // `plugin list` needs no repository; run it from the plugin dir itself.
    let assert = clove(plugin_dir.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "json", "plugin", "list"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true, "envelope: {v}");

    let names: Vec<&str> = v["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"echo"), "echo should be listed: {names:?}");
}
