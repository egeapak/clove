//! cloved: the optional clove daemon (M3), one per user (DESIGN §8.1).
//!
//! For every project it serves it watches `.clove/issues/`, keeps the SQLite
//! index incrementally fresh, answers IPC queries, serves the web UI, and can
//! opt in to git auto-sync. Never required — the CLI works identically without
//! it.
//!
//! Layered as: lifecycle/lock/signals, the hub (per-project slots, handshake),
//! IPC server, file watcher, git auto-sync.

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

#[cfg(feature = "git-sync")]
mod git_sync;
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
    /// Run the daemon in the foreground. It serves every project a client
    /// attaches to, from the per-user runtime directory (`CLOVE_RUNTIME_DIR`).
    Run(RunArgs),
}

#[derive(Debug, clap::Args)]
struct RunArgs {
    /// Load this `.clove/` directory before reporting ready; exit 1 if it
    /// cannot be served.
    #[arg(long)]
    clove_dir: Option<Utf8PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => {
            lifecycle::run(&clove_ipc::HubPaths::resolve(), args.clove_dir.as_deref())
        }
    }
}
