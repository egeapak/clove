//! cloved: the optional clove daemon (M3), one per user (DESIGN §8.1).
//!
//! For every project it serves it watches `.clove/issues/`, keeps the SQLite
//! index incrementally fresh, answers IPC queries, serves the web UI, and can
//! opt in to git auto-sync. Never required — the CLI works identically without
//! it.
//!
//! Layered as: lifecycle/lock/signals, the hub (per-project slots, handshake),
//! IPC server, file watcher, git auto-sync.

use clap::{Parser, Subcommand};

#[cfg(feature = "git-sync")]
mod git_sync;
mod github_remote;
#[cfg(feature = "github-sync")]
mod github_sync;
mod graph_cache;
mod hub;
mod ipc;
mod lifecycle;
mod reindexer;
mod slot;
mod snapshot;
mod state;
mod token;
mod watcher;

/// The `cloved` command line. `clove daemon start` spawns `cloved run` detached
/// (T-D05); end users do not normally invoke this binary directly.
#[derive(Debug, Parser)]
#[command(name = "cloved", version, about = "clove optional daemon (M3)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the daemon in the foreground, from the per-user runtime directory
    /// (`CLOVE_RUNTIME_DIR`). It belongs to no project: every call names its
    /// caller's own.
    Run,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run => lifecycle::run(&clove_ipc::HubPaths::resolve()?),
    }
}
