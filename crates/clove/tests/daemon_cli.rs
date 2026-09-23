//! Phase 4 (T-D05/T-D07): `clove daemon start|stop|status` against the per-user
//! hub, idempotent start, no-op stop, `stop --all`, and the `clove doctor`
//! daemon-health check. Unix-only. Spawns the sibling `cloved` for the
//! start/stop tests; skips those cleanly if it is not built (only outside
//! `cargo test --workspace`). Every test's hub lives in its own temp runtime
//! directory, never the user's.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin;
use assert_cmd::Command;

fn cloved_built() -> bool {
    cargo_bin("clove").with_file_name("cloved").exists()
}

/// `clove` in `dir`, talking to the hub rooted at `run`.
fn clove(dir: &Path, run: &Path) -> Command {
    let mut c = Command::cargo_bin("clove").unwrap();
    c.current_dir(dir)
        .env("CLOVE_RUNTIME_DIR", run)
        .env("CLOVED_DISABLE_WEB", "1");
    c
}

fn init(dir: &Path, run: &Path) {
    clove(dir, run).arg("init").assert().success();
}

fn json(out: &[u8]) -> serde_json::Value {
    serde_json::from_slice(out).unwrap()
}

fn daemon_status(dir: &Path, run: &Path) -> serde_json::Value {
    json(
        &clove(dir, run)
            .args(["daemon", "status", "-f", "json"])
            .output()
            .unwrap()
            .stdout,
    )
}

fn doctor_codes(dir: &Path, run: &Path) -> Vec<String> {
    let out = clove(dir, run)
        .args(["doctor", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    json(&out)["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap().to_owned())
        .collect()
}

/// A test's hub runtime directory; stops any hub still running in it when the
/// test ends, however it ends.
struct Run {
    _tmp: tempfile::TempDir,
    path: PathBuf,
}

impl Run {
    fn new() -> Run {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("run");
        Run { _tmp: tmp, path }
    }

    fn pid_file(&self) -> PathBuf {
        self.path.join("hub.pid")
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        if let Some(pid) = std::fs::read_to_string(self.pid_file())
            .ok()
            .and_then(|p| p.trim().parse::<i32>().ok())
        {
            // SAFETY: kill(2) with a pid our own test hub wrote.
            unsafe {
                libc_kill(pid, 15);
            }
        }
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[test]
fn doctor_flags_and_fixes_stale_daemon_footprints() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    let clove_dir = dir.join(".clove");

    // A crashed clove 0.1.0 daemon: corpse socket + pid in `.clove/`.
    std::fs::write(clove_dir.join("daemon.sock"), b"").unwrap();
    std::fs::write(clove_dir.join("daemon.pid"), b"999999").unwrap();
    // A crashed hub: corpse socket + pid in the runtime directory.
    clove_ipc::ensure_private_dir(camino::Utf8Path::from_path(&run.path).unwrap()).unwrap();
    std::fs::write(run.path.join("hub.sock"), b"").unwrap();
    std::fs::write(run.pid_file(), b"999999").unwrap();

    // doctor (no fix) reports both as warnings, exit 0.
    let codes = doctor_codes(dir, &run.path);
    assert_eq!(
        codes.iter().filter(|c| *c == "DAEMON_STALE_SOCKET").count(),
        2,
        "one finding per corpse footprint, got {codes:?}"
    );

    // --strict with only warnings still exits 0 (warnings are not errors).
    clove(dir, &run.path)
        .args(["doctor", "--strict"])
        .assert()
        .success();

    // --fix removes the corpse files.
    clove(dir, &run.path)
        .args(["doctor", "--fix"])
        .assert()
        .success();
    assert!(!clove_dir.join("daemon.sock").exists());
    assert!(!clove_dir.join("daemon.pid").exists());
    assert!(!run.path.join("hub.sock").exists());
    assert!(!run.pid_file().exists());

    assert!(
        !doctor_codes(dir, &run.path).contains(&"DAEMON_STALE_SOCKET".to_owned()),
        "clean store reports no daemon finding"
    );
}

#[test]
fn stop_with_no_daemon_is_a_clean_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    for args in [
        &["daemon", "stop", "-f", "json"][..],
        &["daemon", "stop", "--all", "-f", "json"][..],
    ] {
        let out = clove(dir, &run.path)
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert_eq!(
            json(&out)["data"]["running"],
            serde_json::json!(false),
            "{args:?}"
        );
    }
}

#[test]
fn start_status_stop_round_trip() {
    if !cloved_built() {
        eprintln!("skipping: cloved not built (run via `cargo test --workspace`)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    clove(dir, &run.path)
        .args(["new", "alpha"])
        .assert()
        .success();

    // start
    let out = clove(dir, &run.path)
        .args(["daemon", "start", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["started"], serde_json::json!(true));

    // status → running with 1 item, and the hub lists this one project
    let v = daemon_status(dir, &run.path);
    assert_eq!(v["data"]["running"], serde_json::json!(true));
    assert_eq!(v["data"]["items_indexed"], serde_json::json!(1));
    assert_eq!(v["data"]["hub"]["projects"].as_array().unwrap().len(), 1);

    // second start is idempotent (already running, exit 0)
    let out = clove(dir, &run.path)
        .args(["daemon", "start", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["running"], serde_json::json!(true));

    // stop → this was the only project, so the hub is torn down too
    clove(dir, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert!(!run.pid_file().exists());
    assert!(!run.path.join("hub.sock").exists());

    let v = daemon_status(dir, &run.path);
    assert_eq!(v["data"]["running"], serde_json::json!(false));
    assert!(v["data"]["hub"].is_null(), "no hub left: {v}");
}

/// #54: `.clove/daemon.sock` in a deeply nested repository overflowed the
/// 104-byte `sun_path`, so the daemon could never bind. The hub's socket lives
/// in the per-user runtime directory, whatever the repository's depth.
#[test]
fn daemon_runs_for_a_repo_nested_past_the_socket_path_limit() {
    if !cloved_built() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let run = Run::new();
    let dir = tmp.path().join("nested-directory/".repeat(8));
    std::fs::create_dir_all(&dir).unwrap();
    assert!(
        dir.join(".clove/daemon.sock").as_os_str().len() > 108,
        "the repo must be deep enough to overflow the old in-repo socket path"
    );
    init(&dir, &run.path);

    clove(&dir, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();
    assert_eq!(
        daemon_status(&dir, &run.path)["data"]["running"],
        serde_json::json!(true)
    );
    clove(&dir, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert_eq!(
        daemon_status(&dir, &run.path)["data"]["running"],
        serde_json::json!(false)
    );
}

/// One daemon serves every project of a user: two repos started from the same
/// runtime directory share one hub process, each is served its own data, and
/// stopping one does not touch the other.
#[test]
fn one_daemon_serves_every_project_independently() {
    if !cloved_built() {
        eprintln!("skipping: cloved not built (run via `cargo test --workspace`)");
        return;
    }
    let t1 = tempfile::tempdir().unwrap();
    let t2 = tempfile::tempdir().unwrap();
    let (d1, d2) = (t1.path(), t2.path());
    let run = Run::new();
    init(d1, &run.path);
    init(d2, &run.path);
    clove(d1, &run.path)
        .args(["new", "only-one"])
        .assert()
        .success();
    clove(d2, &run.path).args(["new", "a"]).assert().success();
    clove(d2, &run.path).args(["new", "b"]).assert().success();

    clove(d1, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();
    let hub_pid = std::fs::read_to_string(run.pid_file()).unwrap();
    clove(d2, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(run.pid_file()).unwrap(),
        hub_pid,
        "the second project joined the running daemon"
    );

    // Each is served its OWN data (distinct item counts), by one process.
    let s1 = daemon_status(d1, &run.path);
    let s2 = daemon_status(d2, &run.path);
    assert_eq!(s1["data"]["running"], serde_json::json!(true));
    assert_eq!(s1["data"]["items_indexed"], serde_json::json!(1));
    assert_eq!(s2["data"]["running"], serde_json::json!(true));
    assert_eq!(s2["data"]["items_indexed"], serde_json::json!(2));
    assert_eq!(s1["data"]["hub"]["projects"].as_array().unwrap().len(), 2);
    assert_eq!(s1["data"]["hub"]["pid"], s2["data"]["hub"]["pid"]);

    // Stopping one leaves the other served, and the daemon running.
    clove(d1, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert_eq!(
        daemon_status(d1, &run.path)["data"]["running"],
        serde_json::json!(false)
    );
    assert_eq!(
        daemon_status(d2, &run.path)["data"]["running"],
        serde_json::json!(true)
    );
    assert!(run.pid_file().exists());

    clove(d2, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
    assert_eq!(
        daemon_status(d2, &run.path)["data"]["running"],
        serde_json::json!(false)
    );
    assert!(!run.pid_file().exists(), "the last stop ends the daemon");
}

#[test]
fn stop_all_stops_the_daemon_for_every_project() {
    if !cloved_built() {
        return;
    }
    let t1 = tempfile::tempdir().unwrap();
    let t2 = tempfile::tempdir().unwrap();
    let (d1, d2) = (t1.path(), t2.path());
    let run = Run::new();
    init(d1, &run.path);
    init(d2, &run.path);
    clove(d1, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();
    clove(d2, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();

    let out = clove(d1, &run.path)
        .args(["daemon", "stop", "--all", "-f", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["data"]["stopped"], serde_json::json!(true));
    assert!(!run.pid_file().exists());
    for dir in [d1, d2] {
        assert_eq!(
            daemon_status(dir, &run.path)["data"]["running"],
            serde_json::json!(false)
        );
    }
}

/// A clove 0.1.0 daemon holds this project's `daemon.lock`: `start` explains why
/// it cannot serve the project, and reads keep working from the files.
#[test]
fn a_project_locked_by_an_older_daemon_falls_back_safely() {
    if !cloved_built() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let run = Run::new();
    init(dir, &run.path);
    clove(dir, &run.path).args(["new", "x"]).assert().success();

    let held = std::fs::File::create(dir.join(".clove/daemon.lock")).unwrap();
    held.try_lock().unwrap();

    let out = clove(dir, &run.path)
        .args(["daemon", "start"])
        .assert()
        .code(7)
        .get_output()
        .stderr
        .clone();
    let stderr = String::from_utf8_lossy(&out);
    assert!(stderr.contains("another daemon"), "{stderr}");

    let v = json(
        &clove(dir, &run.path)
            .args(["ls", "-f", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    assert_ne!(v["_meta"]["source"], serde_json::json!("daemon"));
}

/// The daemon is keyed to the repo's resolved `.clove/` (not the cwd): a project
/// started at the repo root is served — reads included — from any nested
/// subdirectory. This is the same path-resolution that makes git worktrees which
/// share one `.clove/` share one slot.
#[test]
fn daemon_is_reachable_from_any_subdirectory() {
    if !cloved_built() {
        eprintln!("skipping: cloved not built (run via `cargo test --workspace`)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let run = Run::new();
    init(root, &run.path);
    clove(root, &run.path).args(["new", "x"]).assert().success();
    clove(root, &run.path)
        .args(["daemon", "start"])
        .assert()
        .success();

    let sub = root.join("a").join("b").join("c");
    std::fs::create_dir_all(&sub).unwrap();

    let s = daemon_status(&sub, &run.path);
    assert_eq!(s["data"]["running"], serde_json::json!(true));
    assert_eq!(s["data"]["items_indexed"], serde_json::json!(1));

    let v = json(
        &clove(&sub, &run.path)
            .args(["ls", "-f", "json"])
            .output()
            .unwrap()
            .stdout,
    );
    assert_eq!(v["_meta"]["source"], serde_json::json!("daemon"));

    clove(root, &run.path)
        .args(["daemon", "stop"])
        .assert()
        .success();
}
