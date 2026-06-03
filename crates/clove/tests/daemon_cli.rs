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
