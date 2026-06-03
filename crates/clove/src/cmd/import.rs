//! `clove import <tk|beads|github> <src> [--dry-run]` (T-M01/T-M02/T-M03).
//!
//! The tk (T-M01) and Beads (T-M02) sources are implemented; GitHub remains
//! [`CloveError::NotYetImplemented`] until its phase. The shared planning
//! layer lives in `clove-import`: every source runs `plan` (pure, drives
//! `--dry-run`) and, when not in dry-run, `apply` (writes through the file
//! store).

use chrono::Utc;
use clove_core::{CloveError, OutputFormat};
use clove_import::{BeadsImporter, ImportCtx, Importer, TkImporter};
use serde_json::json;

use crate::cli::{ImportArgs, ImportSource};
use crate::context::Ctx;
use crate::output::print_json_success;

pub fn run(ctx: &Ctx, format: OutputFormat, args: ImportArgs) -> Result<(), CloveError> {
    match args.source {
        ImportSource::Tk { src, dry_run } => {
            let importer = TkImporter::new(ctx.config.id_prefix.clone(), Utc::now());
            let import_ctx = ImportCtx::new(&ctx.store, dry_run).map_err(import_err)?;
            let plan = importer.plan(&src, &import_ctx).map_err(import_err)?;

            // Title-fallback (and any other) warnings go to stderr, never stdout,
            // so JSON consumers still get a clean envelope.
            for warning in importer.take_warnings() {
                eprintln!("warning: {warning}");
            }

            if dry_run {
                emit_plan(format, &plan);
            } else {
                let report = importer.apply(plan, &ctx.store).map_err(import_err)?;
                emit_report(format, &report);
            }
            Ok(())
        }
        ImportSource::Beads { src, dry_run } => {
            let importer = BeadsImporter::new(ctx.config.id_prefix.clone(), Utc::now());
            let import_ctx = ImportCtx::new(&ctx.store, dry_run).map_err(import_err)?;
            let plan = importer.plan(&src, &import_ctx).map_err(import_err)?;

            // comment_count (and any other) warnings go to stderr, never stdout,
            // so JSON consumers still get a clean envelope.
            for warning in importer.take_warnings() {
                eprintln!("warning: {warning}");
            }

            if dry_run {
                emit_plan(format, &plan);
            } else {
                let report = importer.apply(plan, &ctx.store).map_err(import_err)?;
                emit_report(format, &report);
            }
            Ok(())
        }
        ImportSource::Github { .. } => Err(CloveError::NotYetImplemented {
            feature: "import github".to_owned(),
        }),
    }
}

/// Emit the `--dry-run` `{ would_create, would_skip, conflicts }` envelope
/// (DESIGN §11.3) in JSON, or a readable summary in human format.
fn emit_plan(format: OutputFormat, plan: &clove_import::ImportPlan) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            serde_json::to_value(plan).unwrap_or_else(|_| json!({})),
            json!({ "warnings": [] }),
        ),
        OutputFormat::Human => {
            println!(
                "dry-run: would create {}, would skip {}, conflicts {}",
                plan.would_create.len(),
                plan.would_skip.len(),
                plan.conflicts.len()
            );
            for item in &plan.would_create {
                println!("  create  {}  {}", item.id, item.title);
            }
            for item in &plan.would_skip {
                println!("  skip    {}  ({})", item.id, item.reason);
            }
        }
    }
}

/// Emit the post-`apply` `{ created, skipped, conflicts }` summary.
fn emit_report(format: OutputFormat, report: &clove_import::ImportReport) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({
                "created": report.created,
                "skipped": report.skipped,
                "conflicts": report.conflicts,
            }),
            json!({ "warnings": [] }),
        ),
        OutputFormat::Human => println!(
            "imported: {} created, {} skipped, {} conflicts",
            report.created, report.skipped, report.conflicts
        ),
    }
}

/// Map a `clove-import` error onto a `CloveError` so the CLI's exit-code mapping
/// applies. `Core` errors pass through unchanged; source/record failures map to
/// the I/O class.
fn import_err(err: clove_import::ImportError) -> CloveError {
    use clove_import::ImportError;
    match err {
        ImportError::Core(core) => core,
        ImportError::NotYetImplemented { feature } => CloveError::NotYetImplemented { feature },
        ImportError::Source { path, message } => CloveError::Io {
            path,
            source: std::io::Error::other(message),
        },
        ImportError::Record { message } => CloveError::Io {
            path: camino::Utf8PathBuf::from("<import source>"),
            source: std::io::Error::other(message),
        },
        other => CloveError::Io {
            path: camino::Utf8PathBuf::from("<import source>"),
            source: std::io::Error::other(other.to_string()),
        },
    }
}
