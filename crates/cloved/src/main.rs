//! cloved: the optional clove daemon (M3).
//!
//! Watches `.clove/issues/`, keeps the SQLite index incrementally fresh, answers
//! IPC queries, and can opt in to git auto-sync. Never required — the CLI works
//! identically without it.
//!
//! Layered as: lifecycle/lock/signals, IPC server, file watcher, git auto-sync.

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

#[cfg(feature = "git-sync")]
mod git_sync;
#[cfg(feature = "github-sync")]
mod github_sync;
mod graph_cache;
mod ipc;
mod lifecycle;
mod reindexer;
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
    /// Run the daemon in the foreground, serving a single `.clove/` directory.
    Run(RunArgs),
}

#[derive(Debug, clap::Args)]
struct RunArgs {
    /// Path to the `.clove/` directory to serve.
    #[arg(long)]
    clove_dir: Utf8PathBuf,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => lifecycle::run(&args.clove_dir),
    }
}
