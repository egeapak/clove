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
    // Pin the clove-managed home into `dir`: `<clove-home>/bin` is on the plugin
    // search path, and `assert_cmd` inherits `$HOME`, so without this a developer
    // with plugins installed under `~/.local/share/clove/bin` would have them
    // resolve inside these tests — silently breaking the "no plugin installed"
    // assertions.
    cmd.env("CLOVE_HOME", dir.join("clove-home"));
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

    // The plugin reports its real binary filename, which carries `.exe` on Windows.
    let expect_binary = format!("clove-sync-echo{}", std::env::consts::EXE_SUFFIX);
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
        assert_eq!(v["data"]["binary"], expect_binary, "mux {mux}: {v}");
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
    // The reported binary carries `.exe` on Windows.
    let exe = std::env::consts::EXE_SUFFIX;
    assert_eq!(reach("import"), format!("clove-import-echo{exe}"));
    assert_eq!(reach("export"), format!("clove-sync-echo{exe}"));
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

#[test]
fn plugin_output_to_a_closed_pipe_exits_quietly() {
    let (plugin_dir, _echo) = install_echo();
    let repo = init_repo("proj");
    // Close the read end before the spawn so the plugin's envelope write hits
    // EPIPE deterministically (`clove echo | head -0`).
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);

    let out = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["--format", "json", "echo", "hello"])
        .stdout(writer)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "status {:?}; stderr: {stderr}",
        out.status
    );
    assert!(!stderr.contains("panicked"), "plugin panicked: {stderr}");
}

/// The host spawns the plugin and exits with the plugin's own exit code
/// (PLUGIN_SYSTEM.md §6.1), not a generic failure.
#[test]
fn the_plugin_exit_code_is_the_host_exit_code() {
    let (plugin_dir, _echo) = install_echo();
    let repo = init_repo("proj");
    let out = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .args(["echo", "--exit-code", "3"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
}

/// Signal handling while the host waits for its plugin (PLUGIN_SYSTEM.md §6.1).
#[cfg(unix)]
mod signals {
    use super::*;
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    use std::time::{Duration, Instant};

    /// Start `clove echo --wait-for-signal` in its own process group and return
    /// the host child and the plugin's pid (read from the pid file it writes).
    // The caller owns the returned child and always waits on it; clippy cannot
    // see across the return.
    #[allow(clippy::zombie_processes)]
    fn start(repo: &Path, plugin_dir: &Path) -> (std::process::Child, i32) {
        let pid_file = repo.join("plugin.pid");
        let mut host = clove(repo)
            .env("CLOVE_PLUGIN_PATH", plugin_dir)
            .args(["echo", "--wait-for-signal"])
            .arg(&pid_file)
            .process_group(0)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse() {
                    return (host, pid);
                }
            }
            if Instant::now() >= deadline {
                let _ = host.kill();
                let _ = host.wait();
                panic!("the plugin never started");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    fn wait_gone(pid: i32) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while alive(pid) {
            assert!(Instant::now() < deadline, "plugin {pid} is still running");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Ctrl-C in a terminal signals the whole foreground group: the plugin dies
    /// of SIGINT, and the host — which survived it — dies of the same signal, so
    /// the shell sees what it would have seen without the host in between.
    #[test]
    fn ctrl_c_to_the_group_ends_both_with_sigint() {
        let (plugin_dir, _echo) = install_echo();
        let repo = init_repo("proj");
        let (mut host, plugin) = start(repo.path(), plugin_dir.path());
        // SAFETY: signalling the test's own child process group.
        unsafe { libc::kill(-(host.id() as i32), libc::SIGINT) };
        let status = host.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
        wait_gone(plugin);
    }

    /// SIGTERM sent to the host pid alone (`kill`, `timeout`, a service
    /// manager) is forwarded to the plugin, and the host then dies of the
    /// plugin's SIGTERM.
    #[test]
    fn sigterm_to_the_host_reaches_the_plugin() {
        let (plugin_dir, _echo) = install_echo();
        let repo = init_repo("proj");
        let (mut host, plugin) = start(repo.path(), plugin_dir.path());
        // SAFETY: signalling the test's own child.
        unsafe { libc::kill(host.id() as i32, libc::SIGTERM) };
        let status = host.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
        wait_gone(plugin);
    }

    /// SIGINT sent to the host pid alone by another process: on Linux it is
    /// forwarded like SIGTERM (`si_code` tells it from a terminal's Ctrl-C);
    /// elsewhere the host absorbs it and the plugin keeps running.
    #[test]
    fn sigint_to_the_host_alone() {
        let (plugin_dir, _echo) = install_echo();
        let repo = init_repo("proj");
        let (mut host, plugin) = start(repo.path(), plugin_dir.path());
        let host_pid = host.id() as i32;
        // SAFETY: signalling the test's own child.
        unsafe { libc::kill(host_pid, libc::SIGINT) };
        if cfg!(target_os = "linux") {
            let status = host.wait().unwrap();
            assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
            wait_gone(plugin);
        } else {
            std::thread::sleep(Duration::from_millis(300));
            assert!(
                host.try_wait().unwrap().is_none(),
                "the host died of SIGINT"
            );
            assert!(alive(plugin), "SIGINT to the host alone reached the plugin");
            // SAFETY: as above.
            unsafe { libc::kill(host_pid, libc::SIGTERM) };
            let status = host.wait().unwrap();
            assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
            wait_gone(plugin);
        }
    }
}

/// Run `clove --format json echo <args>` in `repo` and return the plugin's
/// `data`.
fn echo_json(repo: &Path, plugin_dir: &Path, args: &[&str]) -> Value {
    let out = clove(repo)
        .env("CLOVE_PLUGIN_PATH", plugin_dir)
        .args(["--format", "json", "echo"])
        .args(args)
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {out:?}"));
    assert_eq!(
        v["ok"],
        true,
        "{v}; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    v["data"].clone()
}

/// A published v1 schema, compiled.
fn published_schema(name: &str) -> jsonschema::Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/json-schema/v1")
        .join(name);
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&value).expect("valid schema")
}

/// The host IPC channel (PLUGIN_SYSTEM.md §6.6), end to end: a plugin built on
/// the SDK alone creates, reads, edits, and lists items through the host, which
/// writes them through the unified write path; results have the MCP shapes, and
/// a tool failure carries the code a built-in reports.
#[test]
fn a_plugin_reads_and_writes_items_over_the_host_channel() {
    let (plugin_dir, _echo) = install_echo();
    let repo = init_repo("proj");
    let data = echo_json(repo.path(), plugin_dir.path(), &["--host-roundtrip"]);

    assert_eq!(data["hello"]["host_api"], 1);
    let clove_dir = data["hello"]["clove_dir"].as_str().unwrap();
    assert!(clove_dir.ends_with(".clove"), "{clove_dir}");
    let methods = data["hello"]["methods"].as_array().unwrap();
    for m in ["list", "show", "create", "edit", "set_status", "comment"] {
        assert!(methods.iter().any(|x| x == m), "missing {m}: {methods:?}");
    }

    let id = data["created"]["id"].as_str().expect("created id");
    assert!(id.starts_with("proj-"), "{id}");
    assert_eq!(data["shown"]["title"], "Made over the host channel");
    assert_eq!(data["shown"]["type"], "bug");
    assert_eq!(data["edited"]["status"], "in_progress");

    // The write went to the store: the CLI sees it.
    let out = clove(repo.path())
        .args(["--format", "json", "show", id])
        .output()
        .unwrap();
    let shown: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(shown["data"]["status"], "in_progress", "{shown}");

    // `list` is the MCP item page.
    let page = &data["listed"];
    if let Err(e) = published_schema("mcp-item-page.json").validate(page) {
        panic!("schema violation: {e} in {page}");
    }
    assert_eq!(page["total"], 1, "{page}");

    // A missing id fails as `clove show` would.
    assert_eq!(
        data["missing_code"],
        serde_json::json!({ "clove_code": "ITEM_NOT_FOUND", "exit": 2 })
    );
}

/// A connection that does not present the token is refused.
#[test]
fn the_host_channel_refuses_a_wrong_token() {
    let (plugin_dir, _echo) = install_echo();
    let repo = init_repo("proj");
    let data = echo_json(repo.path(), plugin_dir.path(), &["--host-bad-token"]);
    let refused = data["refused"]
        .as_str()
        .expect("the connection was refused");
    assert!(refused.contains("bad token"), "{refused}");
}

/// A request line over the 16 MiB cap is answered with an error and the
/// connection is closed.
#[test]
fn the_host_channel_rejects_an_oversized_request() {
    let (plugin_dir, _echo) = install_echo();
    let repo = init_repo("proj");
    let data = echo_json(repo.path(), plugin_dir.path(), &["--host-oversize"]);
    let oversize = data["oversize"].as_str().expect("the request failed");
    // The host reads the rest of the line, answers with the reason, then
    // closes. Answering mid-line deadlocked on Windows, where the host's flush
    // waits for a client that is still writing.
    assert!(oversize.contains("16 MiB"), "{oversize}");
    assert!(
        data["after"].is_string(),
        "the connection stayed open: {data}"
    );
}

/// The host's private socket directory is gone once the plugin exits.
#[cfg(unix)]
#[test]
fn the_host_socket_is_removed_after_the_plugin_exits() {
    let (plugin_dir, _echo) = install_echo();
    let repo = init_repo("proj");
    let tmp = tempfile::tempdir().unwrap();
    let out = clove(repo.path())
        .env("CLOVE_PLUGIN_PATH", plugin_dir.path())
        .env("TMPDIR", tmp.path())
        .args(["echo", "hello"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let left: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

/// A connection that never sends `hello` is answered and closed after the
/// 5-second deadline (Unix; Windows named pipes have no receive timeout).
#[cfg(unix)]
#[test]
fn the_host_channel_drops_a_silent_connection() {
    let (plugin_dir, _echo) = install_echo();
    let repo = init_repo("proj");
    let data = echo_json(repo.path(), plugin_dir.path(), &["--host-silent"]);
    let reply = data["reply"].as_str().unwrap();
    assert!(reply.contains("no hello within 5 seconds"), "{reply}");
    let waited = data["waited_ms"].as_u64().unwrap();
    assert!((4_000..30_000).contains(&waited), "waited {waited} ms");
}
