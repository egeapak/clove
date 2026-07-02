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

use std::time::Duration;

use camino::Utf8PathBuf;
use clove_types::ItemType;

pub use engine::Engine;
pub use server::CloveServer;

/// Default heartbeat interval: ping the daemon this often to keep it alive while
/// an MCP session is open. Overridable via `CLOVE_MCP_HEARTBEAT_MS` (tests).
const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(30);

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
    let engine = Engine {
        clove_dir: clove_dir.clone(),
        repo_root,
        id_prefix,
        default_type,
    };

    // Only coordinate through the daemon when the repo actually exists. The
    // server starts even without a `.clove/` (so its tools can report "no
    // repository" rather than the process failing to launch); in that case there
    // is nothing to coordinate, and spawning `cloved` against a missing dir would
    // just waste the readiness wait — and must not materialize a stray `.clove/`.
    if daemon_enabled() && clove_dir.exists() {
        // Bring the coordinator up before serving tools so the first write routes
        // to it; best-effort — tool calls fall back to direct ops if it can't start.
        let _ = clove_ipc::ensure_daemon(&clove_dir);
        spawn_heartbeat(clove_dir);
    }

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(serve(engine))
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

async fn serve(engine: Engine) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let service = CloveServer::new(engine)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
