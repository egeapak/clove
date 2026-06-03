//! cloved: the optional clove daemon (M3).
//!
//! Watches `.clove/issues/`, keeps the SQLite index incrementally fresh, answers
//! IPC queries, and can opt in to git auto-sync. Never required — the CLI works
//! identically without it.
//!
//! **Phase 0 (this commit):** crate scaffolding and the `cloved run` arg surface
//! only. The lifecycle/lock/signal handling (P1), IPC server (P2), file watcher
//! (P3), and git auto-sync (P5) land in subsequent phases per `docs/M3_PLAN.md`.

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

#[cfg(feature = "git-sync")]
mod git_sync;

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
        Command::Run(args) => {
            // Phase 0 skeleton: lifecycle/IPC/watcher land in P1-P3.
            eprintln!(
                "cloved: daemon runtime not yet implemented (Phase 0 skeleton); \
                 would serve {}",
                args.clove_dir
            );
            Ok(())
        }
    }
}
