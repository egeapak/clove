//! Phase 1 (T-D02) lifecycle tests, for the hub: pid-after-bind readiness,
//! clean SIGTERM shutdown with no stale files, the two-hub guard, and idle
//! exit. Unix-only (they drive real signals); the Windows named-event path is
//! covered by the `daemon-windows` CI job.
#![cfg(unix)]

mod support;

use support::{
    command, hold_idle, init_clove_dir, release_idle, TestHub, HANG_GUARD, SIGKILL, SIGTERM,
};

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
    let status = hub.wait_exit(HANG_GUARD).expect("hub exited");
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
    hub.wait_exit(HANG_GUARD).expect("killed");
    assert!(hub.paths.sock().exists(), "corpse socket left behind");

    // A fresh hub must reclaim the lock/socket and become ready again — and the
    // killed hub's project lock died with it.
    let restarted = hub.restart(&clove_dir);
    restarted.signal(SIGTERM);
}

#[test]
#[cfg_attr(not(feature = "test-knobs"), ignore = "needs a `test-knobs` cloved")]
fn an_idle_hub_exits_after_its_projects_idle_out() {
    let (_tmp, clove_dir) = init_clove_dir();
    // CLOVED_IDLE_SHUTDOWN_MS is the test seam for the minute-granularity
    // `[daemon] idle_shutdown_min` (T-D05); CLOVED_HUB_GRACE_MS for the hub's
    // empty-grace period. Both timers are held while the project loads, so
    // neither can fire before the test is ready for it.
    let mut hub = TestHub::spawn_idle_held("500", "200");
    hold_idle(&clove_dir);
    hub.load(&clove_dir).expect("the hub serves the project");
    release_idle(&clove_dir);
    hub.release_hub_idle();

    let status = hub.wait_exit(HANG_GUARD);
    assert!(status.is_some(), "hub self-terminated on idle");
    assert!(status.unwrap().success(), "clean idle shutdown (exit 0)");
    assert!(!hub.paths.pid().exists(), "pid removed on idle shutdown");
    assert!(!hub.paths.sock().exists(), "socket removed");
}

/// Stopping the hub while a project is still loading: once the hub's pid
/// file is gone — which is when `clove daemon stop --all` reports it stopped —
/// the hub has exited and holds neither its own lock nor the project's
/// (L-new-2).
#[test]
#[cfg_attr(not(feature = "test-knobs"), ignore = "needs a `test-knobs` cloved")]
fn a_hub_stopped_mid_load_is_gone_when_its_pid_file_is() {
    let (_a_tmp, a) = init_clove_dir();
    let mut hub = TestHub::spawn_with(
        None,
        &[
            ("CLOVED_DISABLE_WEB", "1"),
            ("CLOVED_LOAD_DELAY_MS", "4000"),
        ],
    );
    let loader = {
        let (paths, a) = (hub.paths.clone(), a.clone());
        std::thread::spawn(move || clove_ipc::DaemonClient::attach(&paths, &a, true).map(|_| ()))
    };
    // Wait until the load holds the project's lock (it then sits on it).
    let project_lock_path = clove_ipc::lock_path(&a);
    assert!(
        support::eventually(HANG_GUARD, || {
            clove_core::fs_safe::open_lock_file(&project_lock_path)
                .is_ok_and(|f| f.try_lock().is_err())
        }),
        "the load never took the project's lock"
    );
    hub.signal(SIGTERM);
    assert!(
        support::eventually(HANG_GUARD, || !hub.paths.pid().exists()),
        "the hub did not stop"
    );
    let exited = hub.wait_exit(HANG_GUARD);
    let hub_lock = clove_core::fs_safe::open_lock_file(&hub.paths.lock()).unwrap();
    let hub_lock_free = hub_lock.try_lock().is_ok();
    let project_lock = clove_core::fs_safe::open_lock_file(&clove_ipc::lock_path(&a)).unwrap();
    let project_lock_free = project_lock.try_lock().is_ok();
    let _ = loader.join();
    assert!(exited.is_some(), "the pid file went but the hub lives on");
    assert!(hub_lock_free, "the hub lock is still held");
    assert!(project_lock_free, "the project's lock is still held");
}

#[test]
fn a_hub_whose_stderr_reader_is_gone_keeps_serving() {
    // Every status line (serving, stopped serving, exiting) goes to stderr.
    // With no reader left those writes fail; they must not panic the hub —
    // release builds abort on a panic.
    let (_tmp, clove_dir) = init_clove_dir();
    let mut hub = TestHub::spawn_with_closed_stderr(&clove_dir);
    hub.client(&clove_dir)
        .status()
        .expect("served after `serving` was printed");
    hub.detach(&clove_dir);
    assert!(
        hub.projects().is_empty(),
        "still answers after `stopped serving`"
    );

    hub.signal(SIGTERM);
    let status = hub.wait_exit(HANG_GUARD).expect("hub exited");
    assert!(
        status.success(),
        "clean exit despite the closed stderr: {status:?}"
    );
}
