//! `clove reindex` (T-S04 CLI half): rebuild the SQLite index from the files.

use clove_core::{CloveError, OutputFormat};
use serde_json::json;

use crate::context::{index_error, Ctx};
use crate::output::print_json_success;

pub fn run(ctx: &Ctx, format: OutputFormat, quiet: bool) -> Result<(), CloveError> {
    let report = clove_index::reindex(&ctx.issues_dir, &ctx.db_path)
        .map_err(|e| index_error(e, &ctx.db_path))?;

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
