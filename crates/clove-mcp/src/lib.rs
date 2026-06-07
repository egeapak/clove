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

use camino::Utf8PathBuf;
use clove_core::ItemType;

pub use engine::Engine;
pub use server::CloveServer;

/// Run the stdio MCP server until the client disconnects.
///
/// Builds a tokio runtime (rmcp is async) and serves on stdin/stdout. `clove_dir`
/// is used to probe the daemon; `repo_root` roots the file store; `id_prefix` and
/// `default_type` configure `clove_new`.
pub fn run(
    clove_dir: Utf8PathBuf,
    repo_root: Utf8PathBuf,
    id_prefix: String,
    default_type: ItemType,
) -> anyhow::Result<()> {
    let engine = Engine {
        clove_dir,
        repo_root,
        id_prefix,
        default_type,
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(serve(engine))
}

async fn serve(engine: Engine) -> anyhow::Result<()> {
    use rmcp::ServiceExt;
    let service = CloveServer::new(engine)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
