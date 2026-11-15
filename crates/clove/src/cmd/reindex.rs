//! `clove reindex` (T-S04 CLI half): rebuild the SQLite index from the files.

use std::time::{Duration, Instant};

use clove_core::OutputFormat;
use clove_ipc::{ClientError, DaemonClient};
use clove_plugin::outln;
use clove_types::CloveError;
use serde_json::json;

use crate::context::{index_error, Ctx};
use crate::output::print_json_success;

/// A reindex report (items, duration, warnings) from whichever side rebuilt.
pub(crate) struct Report {
    items_indexed: u64,
    duration_ms: u64,
    warnings: Vec<String>,
}

pub fn run(ctx: &Ctx, format: OutputFormat, quiet: bool) -> Result<(), CloveError> {
    let report = rebuild(ctx, quiet)?;

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({
                "items_indexed": report.items_indexed,
                "duration_ms": report.duration_ms,
                "warnings": report.warnings,
            }),
            json!({ "warnings": report.warnings }),
        ),
        OutputFormat::Human => {
            if !quiet {
                outln!(
                    "indexed {} item(s) in {} ms",
                    report.items_indexed,
                    report.duration_ms
                );
                for w in &report.warnings {
                    eprintln!("warning: {w}");
                }
            }
        }
    }
    Ok(())
}

/// Rebuild the index: through a running daemon when there is one, else here.
///
/// The daemon rebuilds and reopens its own handle, so the CLI and the daemon
/// stay coherent. A CLI-side rebuild would leave the daemon on the replaced
/// inode until its next reopen. `clove doctor --fix` uses this too.
pub(crate) fn rebuild(ctx: &Ctx, quiet: bool) -> Result<Report, CloveError> {
    match reindex_via_daemon(ctx, quiet) {
        Some(report) => Ok(report),
        None => {
            let r = reindex_locally(ctx)?;
            Ok(Report {
                items_indexed: r.items_indexed as u64,
                duration_ms: r.duration_ms as u64,
                warnings: r.warnings,
            })
        }
    }
}

/// Ask a running daemon to reindex (so it reopens its own handle). `None` → the
/// CLI reindexes locally.
fn reindex_via_daemon(ctx: &Ctx, quiet: bool) -> Option<Report> {
    let clove_dir = ctx.issues_dir.parent()?;
    let mut client = DaemonClient::probe(clove_dir)?;
    match client.reindex() {
        Ok(done) => Some(Report {
            items_indexed: done.items_indexed,
            duration_ms: done.duration_ms,
            warnings: done.warnings,
        }),
        // The daemon went away mid-call — `clove daemon stop`, say. Its
        // rebuild may still be finishing; the local one waits for it.
        Err(ClientError::Transport(_)) => {
            if !quiet {
                eprintln!(
                    "note: the daemon stopped during the reindex; rebuilding here once \
                     its rebuild has finished"
                );
            }
            None
        }
        Err(_) => None,
    }
}

/// How long a local rebuild waits for one already running (a daemon's, cut
/// off from its client) before giving up.
const REBUILD_WAIT: Duration = Duration::from_secs(120);

/// Rebuild here, after any rebuild already running on this index.
fn reindex_locally(ctx: &Ctx) -> Result<clove_index::ReindexReport, CloveError> {
    let start = Instant::now();
    loop {
        match clove_index::reindex(&ctx.issues_dir, &ctx.db_path) {
            Err(clove_index::IndexError::AlreadyRunning) if start.elapsed() < REBUILD_WAIT => {
                if let Some(marker) = crate::knobs::var("CLOVE_REINDEX_WAIT_MARKER") {
                    let _ = std::fs::write(marker, b"waiting");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(clove_index::IndexError::AlreadyRunning) => {
                return Err(CloveError::Io {
                    path: ctx.db_path.clone(),
                    source: std::io::Error::other(format!(
                        "another rebuild of this index (a daemon's, or another \
                         `clove reindex`) has been running for over {}s; try again once \
                         it has finished",
                        REBUILD_WAIT.as_secs()
                    )),
                })
            }
            other => return other.map_err(|e| index_error(e, &ctx.db_path)),
        }
    }
}
