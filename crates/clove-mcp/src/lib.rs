//! clove MCP server (M4): exposes clove to AI agents over the MCP `stdio`
//! transport (newline-delimited JSON-RPC), built on `rmcp`.
//!
//! Architecture (topology B): each MCP client spawns `clove mcp`, which runs this
//! stdio server. Tool **writes** prefer the single `cloved` daemon (serialized +
//! coherent) and fall back to direct `clove-core` ops; **reads** compute from the
//! file store directly. So multiple agents on one project share one write
//! coordinator when a daemon is running, and everything still works without one.

mod args;
mod engine;
mod server;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camino::Utf8PathBuf;
use clove_types::ItemType;
use rmcp::model::ResourceUpdatedNotificationParam;
use rmcp::{Peer, RoleServer};

pub use engine::Engine;
pub use server::{CloveServer, READY_URI, STATS_URI};

/// Default heartbeat interval: ping the daemon this often to keep it alive while
/// an MCP session is open. Overridable via `CLOVE_MCP_HEARTBEAT_MS` (tests).
const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(30);

/// Default notifier poll interval: how often to check the daemon's change
/// generation and push `resources/updated`. Overridable via `CLOVE_MCP_NOTIFY_MS`.
const DEFAULT_NOTIFY: Duration = Duration::from_secs(1);

/// Run the stdio MCP server until the client disconnects.
///
/// Builds a tokio runtime (rmcp is async) and serves on stdin/stdout. `clove_dir`
/// is used to reach the daemon; `repo_root` roots the file store; `id_prefix` and
/// `default_type` configure `clove_new`.
///
/// Topology B: unless `CLOVE_MCP_NO_DAEMON` is set, this **auto-starts** the
/// `cloved` daemon (the write coordinator) and runs a background **heartbeat** that
/// pings it on an interval so it stays alive for the session's duration.
pub fn run(
    clove_dir: Utf8PathBuf,
    repo_root: Utf8PathBuf,
    id_prefix: String,
    default_type: ItemType,
) -> anyhow::Result<()> {
    let engine = Engine::new(clove_dir.clone(), repo_root, id_prefix, default_type);

    // Only coordinate through the daemon when the repo actually exists. The
    // server starts even without a `.clove/` (so its tools can report "no
    // repository" rather than the process failing to launch); in that case there
    // is nothing to coordinate, and spawning `cloved` against a missing dir would
    // just waste the readiness wait — and must not materialize a stray `.clove/`.
    let daemon_active = daemon_enabled() && clove_dir.exists();
    if daemon_active {
        // Bring the coordinator up before serving tools so the first write routes
        // to it; best-effort — tool calls fall back to direct ops if it can't start.
        let _ = clove_ipc::ensure_daemon(&clove_dir);
        spawn_heartbeat(clove_dir.clone());
    }

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(serve(engine, clove_dir, daemon_active))
}

/// Whether to auto-start + heartbeat the daemon (default on; `CLOVE_MCP_NO_DAEMON`
/// opts out — used by the fallback-path tests and power users who never want a
/// daemon spawned).
fn daemon_enabled() -> bool {
    !matches!(
        std::env::var("CLOVE_MCP_NO_DAEMON").ok().as_deref(),
        Some("1") | Some("true")
    )
}

fn heartbeat_interval() -> Duration {
    std::env::var("CLOVE_MCP_HEARTBEAT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_HEARTBEAT)
}

/// Spawn a background thread that keeps the daemon alive: it holds one client and
/// pings on each interval, restarting the daemon if a ping fails. Detached — it
/// dies with the (short-lived, per-session) `clove mcp` process.
fn spawn_heartbeat(clove_dir: Utf8PathBuf) {
    let interval = heartbeat_interval();
    std::thread::spawn(move || {
        let mut client = clove_ipc::ensure_daemon(&clove_dir);
        loop {
            std::thread::sleep(interval);
            let alive = client.as_mut().map(|c| c.ping().is_ok()).unwrap_or(false);
            if !alive {
                client = clove_ipc::ensure_daemon(&clove_dir);
            }
        }
    });
}

fn notify_interval() -> Duration {
    std::env::var("CLOVE_MCP_NOTIFY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_NOTIFY)
}

/// Whether a change from `last` → `current` should fire `resources/updated`.
/// `None` (the first poll) only sets the baseline; any later difference — an
/// increase *or* a decrease (a daemon restart resets the counter) — is a change.
fn should_notify(last: Option<u64>, current: u64) -> bool {
    last.is_some_and(|l| l != current)
}

/// Spawn a background thread that polls the daemon's change-generation and pushes
/// MCP `resources/updated` (for subscribed URIs) + `resources/list_changed` when
/// it increments. The blocking `DaemonClient` runs on the thread; a captured tokio
/// `Handle` fires the async notifications back onto the server runtime. Detached —
/// dies with the per-session `clove mcp` process. Degrades to no-op when the daemon
/// is unreachable (reads still work direct-from-file).
fn spawn_notifier(
    peer: Peer<RoleServer>,
    clove_dir: Utf8PathBuf,
    subscriptions: Arc<Mutex<HashSet<String>>>,
) {
    let interval = notify_interval();
    let handle = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let mut client = clove_ipc::ensure_daemon(&clove_dir);
        let mut last: Option<u64> = None;
        loop {
            std::thread::sleep(interval);
            match client.as_mut().and_then(|c| c.change_generation().ok()) {
                Some(generation) => {
                    if should_notify(last, generation) {
                        // Per-URI `resources/updated` only for subscribed resources.
                        let uris: Vec<String> = subscriptions
                            .lock()
                            .map(|s| s.iter().cloned().collect())
                            .unwrap_or_default();
                        for uri in uris {
                            let peer = peer.clone();
                            handle.spawn(async move {
                                let _ = peer
                                    .notify_resource_updated(ResourceUpdatedNotificationParam {
                                        uri,
                                    })
                                    .await;
                            });
                        }
                        // Coarse belt-and-suspenders for clients that don't do
                        // per-URI subscription (our resource *set* never changes,
                        // so this is a "something changed, re-read" nudge).
                        let peer = peer.clone();
                        handle.spawn(async move {
                            let _ = peer.notify_resource_list_changed().await;
                        });
                    }
                    last = Some(generation);
                }
                // Daemon down/unreachable: keep `last`, re-probe next tick.
                None => client = clove_ipc::ensure_daemon(&clove_dir),
            }
        }
    });
}

async fn serve(engine: Engine, clove_dir: Utf8PathBuf, daemon_active: bool) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let server = CloveServer::new(engine);
    // Grab a handle to the shared subscription set before the server is moved.
    let subscriptions = server.subscriptions();
    let service = server.serve(rmcp::transport::stdio()).await?;
    // Only push notifications when we're coordinating through a daemon (the change
    // signal source). Without one, resources are still readable direct-from-file.
    if daemon_active {
        spawn_notifier(service.peer().clone(), clove_dir, subscriptions);
    }
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::should_notify;

    #[test]
    fn should_notify_baselines_then_fires_on_any_change() {
        assert!(!should_notify(None, 0), "first poll only sets the baseline");
        assert!(!should_notify(None, 7), "first poll never notifies");
        assert!(
            !should_notify(Some(5), 5),
            "unchanged generation → no notify"
        );
        assert!(should_notify(Some(5), 6), "increment → notify");
        assert!(
            should_notify(Some(5), 1),
            "decrease (daemon restart reset) → notify"
        );
    }
}
