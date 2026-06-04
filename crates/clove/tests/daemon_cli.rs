//! Phase 4 (T-D05/T-D07): `clove daemon start|stop|status` lifecycle, idempotent
//! start, no-op stop, and the `clove doctor` daemon-health check. Unix-only.
//! Spawns the sibling `cloved` for the start/stop tests; skips those cleanly if
//! it is not built (only outside `cargo test --workspace`).
#![cfg(unix)]

use std::path::Path;

use assert_cmd::cargo::cargo_bin;
use assert_cmd::Command;

fn cloved_built() -> bool {
    cargo_bin("clove").with_file_name("cloved").exists()
}

fn clove(dir: &Path) -> Command {
    let mut c = Command::cargo_bin("clove").unwrap();
    c.current_dir(dir);
    c
}

fn init(dir: &Path) {
    clove(dir).arg("init").assert().success();
}

fn json(out: &[u8]) -> serde_json::Value {
    serde_json::from_slice(out).unwrap()
}

#[test]
fn doctor_flags_and_fixes_stale_daemon_footprint() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    let clove_dir = dir.join(".clove");
    // Simulate a crashed daemon: corpse socket + pid, nobody listening.
    std::fs::write(clove_dir.join("daemon.sock"), b"").unwrap();
    std::fs::write(clove_dir.join("daemon.pid"), b"999999").unwrap();

    // doctor (no fix) must report the stale footprint as a warning, exit 0.
    let out = clove(dir)
        .args(["doctor", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let codes: Vec<&str> = v["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"DAEMON_STALE_SOCKET"),
        "expected DAEMON_STALE_SOCKET, got {codes:?}"
    );

    // --strict with only this warning still exits 0 (warnings are not errors).
    clove(dir).args(["doctor", "--strict"]).assert().success();

    // --fix removes the corpse files.
    clove(dir).args(["doctor", "--fix"]).assert().success();
    assert!(!clove_dir.join("daemon.sock").exists());
    assert!(!clove_dir.join("daemon.pid").exists());

    // Clean store now reports no daemon finding.
    let out = clove(dir)
        .args(["doctor", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    let codes: Vec<&str> = v["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    assert!(!codes.contains(&"DAEMON_STALE_SOCKET"));
}

#[test]
fn stop_with_no_daemon_is_a_clean_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    let out = clove(dir)
        .args(["daemon", "stop", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["running"], serde_json::json!(false));
}

#[test]
fn start_status_stop_round_trip() {
    if !cloved_built() {
        eprintln!("skipping: cloved not built (run via `cargo test --workspace`)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init(dir);
    clove(dir).args(["new", "alpha"]).assert().success();

    // start
    let out = clove(dir)
        .args(["daemon", "start", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["started"], serde_json::json!(true));

    // status → running with 1 item
    let out = clove(dir)
        .args(["daemon", "status", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["data"]["running"], serde_json::json!(true));
    assert_eq!(v["data"]["items_indexed"], serde_json::json!(1));

    // second start is idempotent (already running, exit 0)
    let out = clove(dir)
        .args(["daemon", "start", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["running"], serde_json::json!(true));

    // stop → torn down
    clove(dir).args(["daemon", "stop"]).assert().success();
    let clove_dir = dir.join(".clove");
    assert!(!clove_dir.join("daemon.pid").exists());
    assert!(!clove_dir.join("daemon.sock").exists());

    // status after stop
    let out = clove(dir)
        .args(["daemon", "status", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["running"], serde_json::json!(false));
}

fn daemon_status(dir: &Path) -> serde_json::Value {
    json(
        &clove(dir)
            .args(["daemon", "status", "-f", "json"])
            .output()
            .unwrap()
            .stdout,
    )
}

/// The daemon is per-project (one per `.clove/` dir), not system-wide: two repos
/// each run their own daemon on their own socket, serving their own data, and
/// stopping one does not touch the other.
#[test]
fn daemons_are_per_project_and_independent() {
    if !cloved_built() {
        eprintln!("skipping: cloved not built (run via `cargo test --workspace`)");
        return;
    }
    let t1 = tempfile::tempdir().unwrap();
    let t2 = tempfile::tempdir().unwrap();
    let (d1, d2) = (t1.path(), t2.path());
    init(d1);
    init(d2);
    clove(d1).args(["new", "only-one"]).assert().success();
    clove(d2).args(["new", "a"]).assert().success();
    clove(d2).args(["new", "b"]).assert().success();

    // A daemon in each project.
    clove(d1).args(["daemon", "start"]).assert().success();
    clove(d2).args(["daemon", "start"]).assert().success();

    // Each serves its OWN data from its OWN socket (distinct item counts).
    let s1 = daemon_status(d1);
    let s2 = daemon_status(d2);
    assert_eq!(s1["data"]["running"], serde_json::json!(true));
    assert_eq!(s1["data"]["items_indexed"], serde_json::json!(1));
    assert_eq!(s2["data"]["running"], serde_json::json!(true));
    assert_eq!(s2["data"]["items_indexed"], serde_json::json!(2));
    assert!(d1.join(".clove/daemon.sock").exists());
    assert!(d2.join(".clove/daemon.sock").exists());

    // Stopping one leaves the other running (isolation).
    clove(d1).args(["daemon", "stop"]).assert().success();
    assert_eq!(
        daemon_status(d1)["data"]["running"],
        serde_json::json!(false)
    );
    assert_eq!(
        daemon_status(d2)["data"]["running"],
        serde_json::json!(true)
    );

    clove(d2).args(["daemon", "stop"]).assert().success();
    assert_eq!(
        daemon_status(d2)["data"]["running"],
        serde_json::json!(false)
    );
}

/// The daemon is keyed to the repo's resolved `.clove/` (not the cwd): a daemon
/// started at the repo root is reachable — and serves reads — from any nested
/// subdirectory. This is the same path-resolution that makes git worktrees which
/// share one `.clove/` share one daemon (and prevents per-subdir sprawl).
#[test]
fn daemon_is_reachable_from_any_subdirectory() {
    if !cloved_built() {
        eprintln!("skipping: cloved not built (run via `cargo test --workspace`)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init(root);
    clove(root).args(["new", "x"]).assert().success();
    clove(root).args(["daemon", "start"]).assert().success();

    let sub = root.join("a").join("b").join("c");
    std::fs::create_dir_all(&sub).unwrap();

    // status from the subdirectory resolves to the same daemon.
    let s = daemon_status(&sub);
    assert_eq!(s["data"]["running"], serde_json::json!(true));
    assert_eq!(s["data"]["items_indexed"], serde_json::json!(1));

    // and a read from the subdirectory is daemon-served.
    let v = json(
        &clove(&sub)
            .args(["ls", "-f", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["_meta"]["source"], serde_json::json!("daemon"));

    clove(root).args(["daemon", "stop"]).assert().success();
}
