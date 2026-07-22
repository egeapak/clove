//! End-to-end tests for Phase 1 of the plugin registry/discovery system
//! (PLUGIN_REGISTRY.md §3/§6): the enriched `clove plugin list` and the dynamic,
//! plugin-aware `<mux> --help`. All offline — no network, no install, no registry.
//!
//! These reuse the `clove-echo` fixture (which answers `--clove-plugin-info` via
//! the `clove-plugin` harness, so it carries the auto-filled compat fields),
//! copied under the multiplexer-scoped name `clove-import-echo`, and point
//! `CLOVE_PLUGIN_PATH` at its temp dir so the search path is deterministic.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// Build the `clove-echo` fixture and copy it into a fresh temp dir under `name`
/// (e.g. `clove-import-echo`), returning `(dir, path)`.
fn install_echo_as(name: &str) -> (TempDir, PathBuf) {
    let built = escargot::CargoBuild::new()
        .package("clove-plugin-echo")
        .bin("clove-echo")
        .run()
        .expect("build clove-echo fixture");
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join(name);
    std::fs::copy(built.path(), &dest).expect("copy echo fixture into the plugin dir");
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
fn plugin_list_json_is_enriched_with_version_provides_status() {
    let (plugin_dir, _echo) = install_echo_as("clove-echo");

    let assert = clove(plugin_dir.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "json", "plugin", "list"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], true, "envelope: {v}");

    let echo = v["data"]
        .as_array()
        .expect("data array")
        .iter()
        .find(|p| p["name"] == "echo")
        .expect("echo listed");

    // Additive over the old `{name,path}`: binary, version, provides, commands,
    // installed, status.
    assert_eq!(echo["binary"], "clove-echo");
    assert!(echo["path"].as_str().unwrap().ends_with("clove-echo"));
    assert!(
        !echo["version"].as_str().unwrap().is_empty(),
        "echo: {echo}"
    );
    assert_eq!(echo["provides"][0], "echo");
    assert_eq!(echo["commands"][0], "clove echo");
    assert_eq!(echo["installed"], true);
    // The echo fixture is built from this same workspace → compatible.
    assert_eq!(echo["status"], "ok");
}

#[test]
fn plugin_list_human_shows_the_enriched_columns() {
    let (plugin_dir, _echo) = install_echo_as("clove-echo");

    let assert = clove(plugin_dir.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["plugin", "list"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(out.contains("NAME"), "header missing: {out}");
    assert!(out.contains("RUN AS"), "header missing: {out}");
    assert!(out.contains("echo"), "echo row missing: {out}");
    assert!(out.contains("clove echo"), "run-as missing: {out}");
}

#[test]
fn import_help_lists_builtins_and_installed_providers() {
    let (plugin_dir, _echo) = install_echo_as("clove-import-echo");
    let repo = init_repo("proj");

    let assert = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["import", "--help"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    // Built-in native formats.
    assert!(out.contains("json"), "json missing: {out}");
    assert!(out.contains("jsonl"), "jsonl missing: {out}");
    // The installed provider line: provider name, the binary, and the run-as.
    assert!(
        out.contains("Installed providers:"),
        "section missing: {out}"
    );
    assert!(out.contains("clove-import-echo"), "binary missing: {out}");
    assert!(out.contains("clove import echo"), "run-as missing: {out}");
    // The globals-precede-provider note is preserved.
    assert!(
        out.contains("must come BEFORE the provider"),
        "note missing: {out}"
    );
}

#[test]
fn sync_help_reports_no_builtin_providers() {
    let repo = init_repo("proj");

    let assert = clove(repo.path())
        .env_remove("CLOVE_PLUGIN_PATH")
        .args(["sync", "--help"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(
        out.contains("none (every provider is a plugin)"),
        "sync builtin note missing: {out}"
    );
}

#[test]
fn provider_help_is_not_intercepted_by_the_dynamic_renderer() {
    // `clove import echo --help` has the help flag PAST the provider, so the
    // dynamic `mux_help` interception does NOT fire (its rule is: the token right
    // after the multiplexer must be the help flag). The proof is that the dynamic
    // "Installed providers:" section — which only the runtime renderer emits — is
    // absent; clap handles the argv along its normal path instead.
    let (plugin_dir, _echo) = install_echo_as("clove-import-echo");
    let repo = init_repo("proj");

    let assert = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["import", "echo", "--help"])
        .assert()
        .success();
    let out = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert!(
        !out.contains("Installed providers:"),
        "dynamic section should be absent (not intercepted): {out}"
    );
}
