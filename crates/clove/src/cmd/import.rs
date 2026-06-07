//! `clove import <tk|beads|github> <src> [--dry-run]` (T-M01/T-M02/T-M03).
//!
//! All three sources are implemented: tk (T-M01), Beads (T-M02), and GitHub
//! (T-M03). The GitHub arm is built behind the `github` feature; without it the
//! command is still recognized but returns a clean fallback error rather than a
//! panic. The shared planning layer lives in `clove-import`: every source runs
//! `plan` (pure, drives `--dry-run`) and, when not in dry-run, `apply` (writes
//! through the file store).

use chrono::Utc;
use clove_core::OutputFormat;
use clove_import::{BeadsImporter, ImportCtx, Importer, TkImporter};
use clove_types::CloveError;
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

            // Title-fallback (and any other) warnings go to stderr for humans and
            // into the JSON envelope's `_meta.warnings` for machine consumers.
            let warnings = importer.take_warnings();
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }

            if dry_run {
                emit_plan(format, &plan, &warnings);
            } else {
                let report = importer.apply(plan, &ctx.store).map_err(import_err)?;
                emit_report(format, &report, &warnings);
            }
            Ok(())
        }
        ImportSource::Beads { src, dry_run } => {
            let importer = BeadsImporter::new(ctx.config.id_prefix.clone(), Utc::now());
            let import_ctx = ImportCtx::new(&ctx.store, dry_run).map_err(import_err)?;
            let plan = importer.plan(&src, &import_ctx).map_err(import_err)?;

            // comment_count (and any other) warnings go to stderr for humans and
            // into the JSON envelope's `_meta.warnings` for machine consumers.
            let warnings = importer.take_warnings();
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }

            if dry_run {
                emit_plan(format, &plan, &warnings);
            } else {
                let report = importer.apply(plan, &ctx.store).map_err(import_err)?;
                emit_report(format, &report, &warnings);
            }
            Ok(())
        }
        ImportSource::Github { src, dry_run } => import_github(ctx, format, &src, dry_run),
    }
}

/// `clove import github <owner/repo> [--dry-run]`.
///
/// The GitHub source is a network endpoint, not a file path, so it does not go
/// through the `Importer` trait — but it keeps the identical dry-run envelope /
/// idempotency / apply-report semantics. A `GITHUB_TOKEN` is required to reach
/// the API (even for `--dry-run`, which still fetches repo state to plan); the
/// network layer errors cleanly when it is absent.
#[cfg(feature = "github")]
fn import_github(
    ctx: &Ctx,
    format: OutputFormat,
    src: &str,
    dry_run: bool,
) -> Result<(), CloveError> {
    let import_ctx = ImportCtx::new(&ctx.store, dry_run).map_err(import_err)?;
    let (plan, report) =
        clove_import::github::import_github(src, &import_ctx, &ctx.store, &ctx.config.id_prefix)
            .map_err(import_err)?;
    match report {
        Some(report) => emit_report(format, &report, &[]),
        None => emit_plan(format, &plan, &[]),
    }
    Ok(())
}

/// When built without the `github` feature the source is recognized but fails
/// with a clean error rather than a parse error.
#[cfg(not(feature = "github"))]
fn import_github(
    _ctx: &Ctx,
    _format: OutputFormat,
    _src: &str,
    _dry_run: bool,
) -> Result<(), CloveError> {
    Err(CloveError::NotYetImplemented {
        feature: "import github (built without github support)".to_owned(),
    })
}

/// Emit the `--dry-run` `{ would_create, would_skip, conflicts }` envelope
/// (DESIGN §11.3) in JSON, or a readable summary in human format.
fn emit_plan(format: OutputFormat, plan: &clove_import::ImportPlan, warnings: &[String]) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            serde_json::to_value(plan).unwrap_or_else(|_| json!({})),
            json!({ "warnings": warnings }),
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
fn emit_report(format: OutputFormat, report: &clove_import::ImportReport, warnings: &[String]) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({
                "created": report.created,
                "skipped": report.skipped,
                "conflicts": report.conflicts,
            }),
            json!({ "warnings": warnings }),
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
