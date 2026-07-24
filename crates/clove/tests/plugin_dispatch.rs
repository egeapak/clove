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
    let dir = tempfile::tempdir().unwrap();
    let dest = install_echo_as(dir.path(), "clove-echo");
    (dir, dest)
}

/// Copy the built `clove-echo` fixture into `dir` under an arbitrary `name` (so the
/// same reflect-everything binary can stand in for `clove-sync-echo`,
/// `clove-import-echo`, … to exercise the umbrella fallback). Returns the dest path.
fn install_echo_as(dir: &Path, name: &str) -> PathBuf {
    static BUILT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let built = BUILT.get_or_init(|| {
        escargot::CargoBuild::new()
            .package("clove-plugin-echo")
            .bin("clove-echo")
            .run()
            .expect("build clove-echo fixture")
            .path()
            .to_path_buf()
    });
    // The host resolver looks for `clove-<provider>{EXE_SUFFIX}`, so the renamed
    // copy must carry the platform executable suffix (`.exe` on Windows).
    let dest = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(built, &dest).expect("copy clove-echo fixture into the plugin dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms).unwrap();
    }
    dest
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

/// The umbrella fallback (PLUGIN_SYSTEM.md §4.2): with only a `clove-sync-echo`
/// installed, `clove import echo` and `clove export echo` both resolve to it, and
/// the plugin sees the *requested* mux in `$CLOVE_COMMAND` (so a real
/// multi-capability binary would branch on it).
#[test]
fn umbrella_fallback_routes_import_and_export_to_sync_binary() {
    let plugin_dir = tempfile::tempdir().unwrap();
    install_echo_as(plugin_dir.path(), "clove-sync-echo");
    let repo = init_repo("proj");

    for (mux, expect) in [("import", "import"), ("export", "export"), ("sync", "sync")] {
        let assert = clove(repo.path())
            .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
            .args(["--format", "json", mux, "echo", "arg1"])
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
        assert_eq!(v["ok"], true, "envelope: {v}");
        assert_eq!(v["data"]["command"], expect, "mux {mux}: {v}");
        assert_eq!(v["data"]["provider"], "echo", "mux {mux}: {v}");
        assert_eq!(v["data"]["binary"], "clove-sync-echo", "mux {mux}: {v}");
        let argv = v["data"]["argv"].as_array().unwrap();
        assert!(argv.iter().any(|a| a == "arg1"), "argv {argv:?}");
    }
}

/// Precedence (PLUGIN_SYSTEM.md §4.2/§4.3): a dedicated `clove-import-echo` wins
/// over the `clove-sync-echo` umbrella for `import echo`, while `export echo`
/// (which has no dedicated binary) still falls back to the sync umbrella.
#[test]
fn dedicated_binary_wins_over_umbrella() {
    let plugin_dir = tempfile::tempdir().unwrap();
    install_echo_as(plugin_dir.path(), "clove-sync-echo");
    install_echo_as(plugin_dir.path(), "clove-import-echo");
    let repo = init_repo("proj");

    let reach = |mux: &str| -> String {
        let assert = clove(repo.path())
            .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
            .args(["--format", "json", mux, "echo"])
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
        v["data"]["binary"].as_str().unwrap().to_owned()
    };

    // Dedicated import binary wins; export has no dedicated binary → sync umbrella.
    assert_eq!(reach("import"), "clove-import-echo");
    assert_eq!(reach("export"), "clove-sync-echo");
}

/// A plugin reached via the umbrella fallback for a capability outside its
/// `provides` set rejects it with the standard exit-2 `UNSUPPORTED_CAPABILITY`
/// envelope (PLUGIN_SYSTEM.md §4.2). `clove-import-tk` is import-only, so an
/// `export tk` cross-sibling dispatch must be refused cleanly.
#[test]
fn unsupported_capability_is_exit_2() {
    let built = escargot::CargoBuild::new()
        .package("clove-import-tk")
        .bin("clove-import-tk")
        .run()
        .expect("build clove-import-tk");
    let plugin_dir = tempfile::tempdir().unwrap();
    // The renamed copy must carry the platform executable suffix (`.exe` on Windows)
    // for the host resolver to find it.
    let dest = plugin_dir
        .path()
        .join(format!("clove-import-tk{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(built.path(), &dest).unwrap();
    let repo = init_repo("proj");

    let assert = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        // export → no clove-export-tk, no clove-sync-tk, but clove-import-tk exists
        // (cross-sibling). tk is import-only, so it refuses `export tk`.
        .args(["--format", "json", "export", "tk", "some-dir"])
        .assert()
        .failure()
        .code(2);
    let v: Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    assert_eq!(v["ok"], false, "envelope: {v}");
    assert_eq!(v["error"]["code"], "UNSUPPORTED_CAPABILITY", "{v}");
    assert_eq!(v["error"]["exit"], 2, "{v}");
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
