//! Phase 2 (T-D03) end-to-end: `clove ls`/`ready`/`query` route through a running
//! `cloved` and produce output identical (bar `_meta.source`) to the local path.
//! Unix-only (drives real signals). Spawns the sibling `cloved` binary from the
//! same target dir; skips cleanly if it is not built (only happens outside the
//! `cargo test --workspace` gate).
#![cfg(unix)]
#![allow(clippy::zombie_processes)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use assert_cmd::cargo::cargo_bin;

fn cloved_bin() -> Option<PathBuf> {
    let path = cargo_bin("clove").with_file_name("cloved");
    path.exists().then_some(path)
}

fn clove() -> Command {
    Command::new(cargo_bin("clove"))
}

fn run_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    clove().current_dir(dir).args(args).output().unwrap()
}

fn spawn_daemon(clove_dir: &std::path::Path, bin: &std::path::Path) -> Child {
    let child = Command::new(bin)
        .arg("run")
        .arg("--clove-dir")
        .arg(clove_dir)
        .spawn()
        .expect("spawn cloved");
    let pid = clove_dir.join("daemon.pid");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if pid.exists() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon not ready");
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
fn sigterm(pid: u32) {
    unsafe {
        libc_kill(pid as i32, 15);
    }
}

/// Parse the `data` ids and `_meta.source` from a `--format json` list output.
fn ids_and_source(out: &[u8]) -> (Vec<String>, String) {
    let v: serde_json::Value = serde_json::from_slice(out).unwrap();
    let ids = v["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_owned())
        .collect();
    let source = v["_meta"]["source"].as_str().unwrap_or("").to_owned();
    (ids, source)
}

#[test]
fn ls_ready_query_route_through_daemon_with_parity() {
    let Some(bin) = cloved_bin() else {
        eprintln!("skipping: cloved binary not built (run via `cargo test --workspace`)");
        return;
    };

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert!(run_in(root, &["init"]).status.success());
    for title in ["alpha", "beta", "gamma"] {
        assert!(run_in(root, &["new", title]).status.success());
    }
    assert!(run_in(root, &["reindex"]).status.success());

    let clove_dir = root.join(".clove");
    let mut daemon = spawn_daemon(&clove_dir, &bin);

    // Ground truth: the file-scan path (no index, no daemon).
    let (mut want_ls, _) =
        ids_and_source(&run_in(root, &["--no-index", "ls", "-f", "json"]).stdout);
    want_ls.sort();

    for (cmd, args) in [
        ("ls", vec!["ls", "-f", "json"]),
        ("ready", vec!["ready", "-f", "json"]),
        ("query", vec!["query", "-f", "json"]),
    ] {
        let (mut ids, source) = ids_and_source(&run_in(root, &args).stdout);
        assert_eq!(source, "daemon", "{cmd} must be served by the daemon");
        ids.sort();
        assert_eq!(
            ids, want_ls,
            "{cmd} via daemon must match the file-scan id set"
        );
    }

    sigterm(daemon.id());
    let _ = daemon.wait();

    // With the daemon gone, the same read falls back cleanly (index path) and the
    // stale socket/pid are cleaned up by the liveness probe.
    let (_, source) = ids_and_source(&run_in(root, &["ls", "-f", "json"]).stdout);
    assert_ne!(source, "daemon", "no daemon → fall back");
    assert!(
        !clove_dir.join("daemon.sock").exists(),
        "stale sock cleaned"
    );
}
