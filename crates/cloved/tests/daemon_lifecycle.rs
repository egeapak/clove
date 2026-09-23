//! Phase 1 (T-D02) lifecycle tests, for the hub: pid-after-bind readiness,
//! clean SIGTERM shutdown with no stale files, the two-hub guard, and idle
//! exit. Unix-only (they drive real signals); the Windows named-event path is
//! covered by the `daemon-windows` CI job.
#![cfg(unix)]

mod support;

use std::time::Duration;

use support::{command, init_clove_dir, TestHub, SIGKILL, SIGTERM};

#[test]
fn hub_pid_appears_only_after_socket_is_bound() {
    let (_tmp, clove_dir) = init_clove_dir();
    let hub = TestHub::spawn(Some(&clove_dir));
    // Readiness invariant: when the pid exists, the socket must already exist.
    assert!(hub.paths.pid().exists());
    assert!(
        hub.paths.sock().exists(),
        "socket must be bound before the pid is written"
    );
    // …and the preloaded project is already served.
    hub.client(&clove_dir);
    hub.signal(SIGTERM);
}

#[test]
fn sigterm_shuts_down_cleanly_with_no_stale_files() {
    let (_tmp, clove_dir) = init_clove_dir();
    let mut hub = TestHub::spawn(Some(&clove_dir));

    hub.signal(SIGTERM);
    let status = hub.wait_exit(Duration::from_secs(5)).expect("hub exited");
    assert!(status.success(), "clean SIGTERM exit (exit 0)");

    assert!(!hub.paths.sock().exists(), "socket removed");
    assert!(!hub.paths.pid().exists(), "pid removed");
    // The project's lock is released with the hub: a new hub can serve it.
    let again = TestHub::spawn(Some(&clove_dir));
    again.client(&clove_dir);
}

#[test]
fn second_hub_refuses_to_start() {
    let (_tmp, clove_dir) = init_clove_dir();
    let first = TestHub::spawn(Some(&clove_dir));

    // A second hub in the same runtime directory must fail fast (lock held).
    let second = command(&first.paths, &[("CLOVED_DISABLE_WEB", "1")])
        .output()
        .expect("run second cloved");
    assert!(
        !second.status.success(),
        "second hub must exit non-zero; stderr={}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already running"),
        "expected 'already running' message"
    );
    first.client(&clove_dir);
}

/// A second hub under *another* runtime directory cannot take a project the
/// first already serves: the project's `daemon.lock` decides.
#[test]
fn a_served_project_cannot_be_loaded_by_another_hub() {
    let (_tmp, clove_dir) = init_clove_dir();
    let first = TestHub::spawn(Some(&clove_dir));
    let second = TestHub::spawn(None);
    match second.load(&clove_dir) {
        Err(clove_ipc::ClientError::Refused { code, message }) => {
            assert_eq!(code, clove_ipc::hub::codes::PROJECT_LOCKED);
            assert!(message.contains("daemon.lock"), "{message}");
        }
        Err(other) => panic!("expected PROJECT_LOCKED, got {other}"),
        Ok(_) => panic!("the second hub served a locked project"),
    }
    first.client(&clove_dir);
}

#[test]
fn sigkill_then_restart_recovers() {
    let (_tmp, clove_dir) = init_clove_dir();
    let mut hub = TestHub::spawn(Some(&clove_dir));
    // Hard-kill: leaves a corpse socket + pid (no clean shutdown).
    hub.signal(SIGKILL);
    hub.wait_exit(Duration::from_secs(5)).expect("killed");
    assert!(hub.paths.sock().exists(), "corpse socket left behind");

    // A fresh hub must reclaim the lock/socket and become ready again — and the
    // killed hub's project lock died with it.
    let restarted = hub.restart(&clove_dir);
    restarted.signal(SIGTERM);
}

#[test]
fn an_idle_hub_exits_after_its_projects_idle_out() {
    let (_tmp, clove_dir) = init_clove_dir();
    // CLOVED_IDLE_SHUTDOWN_MS is the test seam for the minute-granularity
    // `[daemon] idle_shutdown_min` (T-D05); CLOVED_HUB_GRACE_MS for the hub's
    // empty-grace period.
    let mut hub = TestHub::spawn_with(
        Some(&clove_dir),
        &[
            ("CLOVED_DISABLE_WEB", "1"),
            ("CLOVED_IDLE_SHUTDOWN_MS", "500"),
            ("CLOVED_HUB_GRACE_MS", "200"),
        ],
    );

    let status = hub.wait_exit(Duration::from_secs(5));
    assert!(status.is_some(), "hub self-terminated on idle");
    assert!(status.unwrap().success(), "clean idle shutdown (exit 0)");
    assert!(!hub.paths.pid().exists(), "pid removed on idle shutdown");
    assert!(!hub.paths.sock().exists(), "socket removed");
}
