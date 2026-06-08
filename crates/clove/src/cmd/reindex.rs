//! `clove reindex` (T-S04 CLI half): rebuild the SQLite index from the files.

use clove_core::OutputFormat;
use clove_ipc::DaemonClient;
use clove_types::CloveError;
use serde_json::json;

use crate::context::{index_error, Ctx};
use crate::output::print_json_success;

/// A reindex report (items, duration, warnings) from whichever side rebuilt.
struct Report {
    items_indexed: u64,
    duration_ms: u64,
    warnings: Vec<String>,
}

pub fn run(ctx: &Ctx, format: OutputFormat, quiet: bool) -> Result<(), CloveError> {
    // Delegate to a running daemon: it rebuilds and reopens its own handle, so
    // the CLI and daemon stay coherent (a CLI-side rebuild would leave the
    // daemon pointing at the replaced inode until its next reopen).
    let report = match reindex_via_daemon(ctx) {
        Some(report) => report,
        None => {
            let r = clove_index::reindex(&ctx.issues_dir, &ctx.db_path)
                .map_err(|e| index_error(e, &ctx.db_path))?;
            Report {
                items_indexed: r.items_indexed as u64,
                duration_ms: r.duration_ms as u64,
                warnings: r.warnings,
            }
        }
    };

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
                println!(
                    "indexed {} item(s) in {} ms",
                    report.items_indexed, report.duration_ms
                );
                for w in &report.warnings {
                    eprintln!("warning: {w}");
                }
            }
        }
    }
    Ok(())
}

/// Ask a running daemon to reindex (so it reopens its own handle). `None` → the
/// CLI reindexes locally.
fn reindex_via_daemon(ctx: &Ctx) -> Option<Report> {
    let clove_dir = ctx.issues_dir.parent()?;
    let mut client = DaemonClient::probe(clove_dir)?;
    let done = client.reindex().ok()?;
    Some(Report {
        items_indexed: done.items_indexed,
        duration_ms: done.duration_ms,
        warnings: done.warnings,
    })
}
