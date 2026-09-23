//! The hub's exit decision races new work: stopping the last project while
//! another client starts a project must never leave the new one stranded on a
//! hub that is about to exit (H1). Unix-only.
#![cfg(unix)]

mod support;

use std::time::{Duration, Instant};

use clove_ipc::DaemonClient;
use support::{init_clove_dir, runtime_dir};

/// Stops whatever hub is left in the runtime directory when the test ends.
struct KillHub(clove_ipc::HubPaths);

impl Drop for KillHub {
    fn drop(&mut self) {
        if let Some(pid) = self.0.read_pid() {
            support::send_signal(pid, support::SIGTERM);
        }
    }
}

fn wait_gone(hub: &clove_ipc::HubPaths) {
    let start = Instant::now();
    while hub.pid().exists() && start.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Clients poll a hub's lock to learn whether one is alive; a hub starting at
/// that instant must not mistake the poll for another hub and give up (N1).
#[test]
fn a_liveness_poll_never_makes_a_starting_hub_give_up() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let (_run, hub) = runtime_dir();
    let _kill = KillHub(hub.clone());
    let polling = Arc::new(AtomicBool::new(true));
    let pollers: Vec<_> = (0..4)
        .map(|_| {
            let (hub, polling) = (hub.clone(), polling.clone());
            std::thread::spawn(move || {
                while polling.load(Ordering::Relaxed) {
                    let _ = hub.running();
                }
            })
        })
        .collect();

    let mut gave_up = Vec::new();
    for round in 0..25 {
        let mut child = support::command(&hub, &[("CLOVED_DISABLE_WEB", "1")])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn cloved");
        let start = Instant::now();
        let exited = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if hub.pid().exists() || start.elapsed() > Duration::from_secs(10) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        if let Some(status) = exited {
            let mut stderr = String::new();
            use std::io::Read as _;
            let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);
            gave_up.push(format!("round {round}: {status}: {}", stderr.trim()));
            continue;
        }
        support::send_signal(child.id(), support::SIGTERM);
        let _ = child.wait();
        wait_gone(&hub);
    }
    polling.store(false, Ordering::Relaxed);
    for poller in pollers {
        let _ = poller.join();
    }
    assert!(gave_up.is_empty(), "{}", gave_up.join("\n"));
}

#[test]
fn stopping_the_last_project_while_another_starts_never_strands_it() {
    // `ensure_daemon_at` spawns `cloved` from `CLOVED_PATH`; set before any
    // thread exists.
    std::env::set_var("CLOVED_PATH", support::cloved_bin());
    // The hubs this test spawns inherit it: token records stay in the build.
    std::env::set_var("CLOVE_HOME", support::TEST_CLOVE_HOME);
    std::env::set_var("CLOVED_DISABLE_WEB", "1");
    let (_ta, a) = init_clove_dir();
    let (_tb, b) = init_clove_dir();
    let (_run, hub) = runtime_dir();
    let _kill = KillHub(hub.clone());

    let mut failures = Vec::new();
    for round in 0..25 {
        clove_ipc::ensure_daemon_at(&hub, &a).expect("A starts");
        // Half the rounds start B while A's stop is still in flight, half right
        // after it returns — inside the window where the hub has decided to
        // exit but not yet exited.
        let started = if round % 2 == 0 {
            let (stop_hub, stop_dir) = (hub.clone(), a.clone());
            let stopper = std::thread::spawn(move || {
                DaemonClient::probe_at(&stop_hub, &stop_dir).map(|mut c| c.detach())
            });
            let started = clove_ipc::ensure_daemon_at(&hub, &b);
            let _ = stopper.join();
            started
        } else {
            let _ = DaemonClient::probe_at(&hub, &a).map(|mut c| c.detach());
            std::thread::sleep(Duration::from_millis((round % 5) as u64 * 10));
            clove_ipc::ensure_daemon_at(&hub, &b)
        };
        match started {
            Err(e) => failures.push(format!("round {round}: start failed: {e}")),
            Ok(_) => {
                std::thread::sleep(Duration::from_millis(300));
                if DaemonClient::probe_at(&hub, &b).is_none() {
                    failures.push(format!("round {round}: B was stranded"));
                }
            }
        }
        for dir in [&b, &a] {
            if let Some(mut client) = DaemonClient::probe_at(&hub, dir) {
                let _ = client.detach();
            }
        }
        wait_gone(&hub);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
