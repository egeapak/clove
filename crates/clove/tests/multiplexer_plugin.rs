//! End-to-end tests for the `import`/`export` multiplexer plugin fall-through
//! (PLUGIN_SYSTEM.md §4.2/§4.3, Phase 3).
//!
//! Built-in providers (`tk`/`beads`, `json`/`jsonl`) stay in-process; any other
//! provider resolves a `clove-<multiplexer>-<provider>` binary on the search
//! path. These tests reuse the `clove-echo` fixture — copied under the
//! multiplexer-scoped names `clove-import-echo` / `clove-export-echo` — to prove
//! the host resolves it, forwards `rest`, and threads `command`/`provider` into
//! the exported env. A provider with no matching plugin is a scoped validation
//! error (exit 4) that names the binary to install.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

/// Build the `clove-echo` fixture and copy it into a fresh temp dir under the
/// given multiplexer-scoped `name` (e.g. `clove-import-echo`), returning
/// `(dir, path)`. The temp dir is what `CLOVE_PLUGIN_PATH` points at so the
/// search path is deterministic.
fn install_echo_as(name: &str) -> (TempDir, PathBuf) {
    let built = escargot::CargoBuild::new()
        .package("clove-plugin-echo")
        .bin("clove-echo")
        .run()
        .expect("build clove-echo fixture");
    let dir = tempfile::tempdir().unwrap();
    // The host resolver looks for `clove-<provider>{EXE_SUFFIX}`, so the renamed
    // copy must carry the platform executable suffix (`.exe` on Windows).
    let dest = dir
        .path()
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
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
fn import_external_provider_is_dispatched_with_argv_and_env() {
    let (plugin_dir, _echo) = install_echo_as("clove-import-echo");
    let repo = init_repo("proj");

    // `--format json` precedes the provider so the host parses it; everything
    // after the provider (`foo --bar`) is forwarded raw to the plugin.
    let assert = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "json", "import", "echo", "foo", "--bar"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(v["ok"], true, "envelope: {v}");
    // The multiplexer + provider are threaded through the env, not argv.
    assert_eq!(v["data"]["command"], "import");
    assert_eq!(v["data"]["provider"], "echo");
    // `rest` is forwarded verbatim (the leading `import echo` echo is stripped by
    // the plugin harness).
    let argv = v["data"]["argv"].as_array().expect("data.argv array");
    assert!(argv.iter().any(|a| a == "foo"), "argv: {argv:?}");
    assert!(argv.iter().any(|a| a == "--bar"), "argv: {argv:?}");
    // Discovery ran: the exported CLOVE_DIR points inside the repo.
    let clove_dir = v["data"]["clove_dir"].as_str().unwrap();
    assert!(clove_dir.ends_with(".clove"), "clove_dir: {clove_dir}");
    assert_eq!(v["data"]["id_prefix"], "proj");
}

#[test]
fn export_external_provider_is_dispatched_with_argv_and_env() {
    let (plugin_dir, _echo) = install_echo_as("clove-export-echo");
    let repo = init_repo("proj");

    let assert = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "json", "export", "echo", "out.csv", "--flag"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();

    assert_eq!(v["ok"], true, "envelope: {v}");
    assert_eq!(v["data"]["command"], "export");
    assert_eq!(v["data"]["provider"], "echo");
    let argv = v["data"]["argv"].as_array().expect("data.argv array");
    assert!(argv.iter().any(|a| a == "out.csv"), "argv: {argv:?}");
    assert!(argv.iter().any(|a| a == "--flag"), "argv: {argv:?}");
}

#[test]
fn unknown_import_provider_is_a_scoped_validation_error() {
    // No CLOVE_PLUGIN_PATH → `clove-import-nope` resolves nowhere → exit 4 with an
    // install hint scoped to the multiplexer (never a fall-back to `clove-nope`).
    let repo = init_repo("proj");
    let assert = clove(repo.path())
        .env_remove("CLOVE_PLUGIN_PATH")
        .args(["--format", "json", "import", "nope"])
        .assert()
        .failure()
        .code(4);
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "VALIDATION_ERROR");
    assert_eq!(v["error"]["exit"], 4);
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("unknown import provider") && msg.contains("install clove-import-nope"),
        "message: {msg}"
    );
}

#[test]
fn unknown_export_provider_is_a_scoped_validation_error() {
    let repo = init_repo("proj");
    let assert = clove(repo.path())
        .env_remove("CLOVE_PLUGIN_PATH")
        .args(["--format", "json", "export", "nope"])
        .assert()
        .failure()
        .code(4);
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "VALIDATION_ERROR");
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("unknown export provider") && msg.contains("install clove-export-nope"),
        "message: {msg}"
    );
}
