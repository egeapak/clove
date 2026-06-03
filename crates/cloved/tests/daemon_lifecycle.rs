//! Phase 1 (T-D02) lifecycle tests: pid-after-bind readiness, clean SIGTERM
//! shutdown with no stale files, and the two-daemon guard. Unix-only (they drive
//! real signals); the Windows named-event path is covered by the `daemon-windows`
//! CI job.
#![cfg(unix)]
// These tests deliberately manage child-process lifetimes by hand (spawn, signal,
// wait/kill on specific paths); the heuristic zombie-process lint can't see it.
#![allow(clippy::zombie_processes)]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use camino::{Utf8Path, Utf8PathBuf};

/// Path to the freshly built `cloved` binary under test.
fn cloved_bin() -> Utf8PathBuf {
    Utf8PathBuf::from(env!("CARGO_BIN_EXE_cloved"))
}

/// Create a minimal `.clove/` directory (config + issues + an index) good enough
/// for the daemon to open.
fn init_clove_dir() -> (tempfile::TempDir, Utf8PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8Path::from_path(dir.path()).unwrap().to_owned();
    let clove_dir = root.join(".clove");
    std::fs::create_dir_all(clove_dir.join("issues")).unwrap();
    std::fs::write(
        clove_dir.join("config.toml"),
        "schema = 1\nid_prefix = \"proj\"\n",
    )
    .unwrap();
    (dir, clove_dir)
}

/// Spawn `cloved run --clove-dir <dir>` and wait until it writes its pid file
/// (its readiness signal), up to `timeout`.
fn spawn_ready(clove_dir: &Utf8Path, timeout: Duration) -> Child {
    let child = Command::new(cloved_bin())
        .arg("run")
        .arg("--clove-dir")
        .arg(clove_dir.as_str())
        .spawn()
        .expect("spawn cloved");
    let pid_file = clove_dir.join("daemon.pid");
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pid_file.exists() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon did not become ready (no pid file) within {timeout:?}");
}

fn send_signal(pid: u32, sig: i32) {
    // SAFETY: `kill(2)` with a pid we spawned and a constant signal number.
    unsafe {
        libc_kill(pid as i32, sig);
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

#[test]
fn daemon_pid_appears_only_after_socket_is_bound() {
    let (_tmp, clove_dir) = init_clove_dir();
    let mut child = spawn_ready(&clove_dir, Duration::from_secs(5));
    // Readiness invariant: when the pid exists, the socket must already exist.
    assert!(clove_dir.join("daemon.pid").exists());
    assert!(
        clove_dir.join("daemon.sock").exists(),
        "socket must be bound before the pid is written"
    );
    send_signal(child.id(), SIGTERM);
    let _ = child.wait();
}

#[test]
fn sigterm_shuts_down_cleanly_with_no_stale_files() {
    let (_tmp, clove_dir) = init_clove_dir();
    let mut child = spawn_ready(&clove_dir, Duration::from_secs(5));
    let pid = child.id();

    send_signal(pid, SIGTERM);
    let status = wait_with_timeout(&mut child, Duration::from_secs(5)).expect("daemon exited");
    assert!(status.success(), "clean SIGTERM exit (exit 0)");

    assert!(!clove_dir.join("daemon.sock").exists(), "socket removed");
    assert!(!clove_dir.join("daemon.pid").exists(), "pid removed");
}

#[test]
fn second_daemon_refuses_to_start() {
    let (_tmp, clove_dir) = init_clove_dir();
    let mut first = spawn_ready(&clove_dir, Duration::from_secs(5));

    // A second daemon on the same .clove must fail fast (lock held).
    let second = Command::new(cloved_bin())
        .arg("run")
        .arg("--clove-dir")
        .arg(clove_dir.as_str())
        .output()
        .expect("run second cloved");
    assert!(
        !second.status.success(),
        "second daemon must exit non-zero; stderr={}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already running"),
        "expected 'already running' message"
    );

    send_signal(first.id(), SIGTERM);
    let _ = first.wait();
}

#[test]
fn sigkill_then_restart_recovers() {
    let (_tmp, clove_dir) = init_clove_dir();
    let mut child = spawn_ready(&clove_dir, Duration::from_secs(5));
    // Hard-kill: leaves a corpse socket + pid (no clean shutdown).
    send_signal(child.id(), SIGKILL);
    let _ = child.wait();

    // A fresh daemon must reclaim the lock/socket and become ready again.
    let mut restarted = spawn_ready(&clove_dir, Duration::from_secs(5));
    assert!(clove_dir.join("daemon.sock").exists());
    send_signal(restarted.id(), SIGTERM);
    let _ = restarted.wait();
}

#[test]
fn idle_shutdown_self_terminates() {
    let (_tmp, clove_dir) = init_clove_dir();
    // CLOVED_IDLE_SHUTDOWN_MS is the test seam for the minute-granularity
    // `[daemon] idle_shutdown_min` (T-D05): self-terminate after 500ms idle.
    let mut child = Command::new(cloved_bin())
        .arg("run")
        .arg("--clove-dir")
        .arg(clove_dir.as_str())
        .env("CLOVED_IDLE_SHUTDOWN_MS", "500")
        .spawn()
        .expect("spawn cloved");

    // Wait for readiness.
    let pid_file = clove_dir.join("daemon.pid");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) && !pid_file.exists() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(pid_file.exists(), "daemon became ready");

    // With no activity it must self-terminate cleanly within a few windows.
    let status = wait_with_timeout(&mut child, Duration::from_secs(3));
    assert!(status.is_some(), "daemon self-terminated on idle");
    assert!(status.unwrap().success(), "clean idle shutdown (exit 0)");
    assert!(!pid_file.exists(), "pid removed on idle shutdown");
    assert!(!clove_dir.join("daemon.sock").exists(), "socket removed");
}

/// Wait for `child` up to `timeout`, returning its exit status or `None` on
/// timeout (after which it is killed).
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        match child.try_wait().unwrap() {
            Some(status) => return Some(status),
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let _ = child.kill();
    None
}
