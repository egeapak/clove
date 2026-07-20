//! `clove import <tk|beads|github> <src> [--dry-run]` (T-M01/T-M02/T-M03).
//!
//! All three sources are implemented: tk (T-M01), Beads (T-M02), and GitHub
//! (T-M03). The GitHub arm is built behind the `github` feature; without it the
//! command is still recognized but returns a clean fallback error rather than a
//! panic. The shared planning layer lives in `clove-import`: every source runs
//! `plan` (pure, drives `--dry-run`) and, when not in dry-run, `apply` (writes
//! through the file store).

use camino::Utf8PathBuf;
use chrono::Utc;
use clap::error::ErrorKind;
use clap::Parser;
use clove_core::OutputFormat;
use clove_import::{BeadsImporter, ImportCtx, Importer, TkImporter};
use clove_types::CloveError;
use serde_json::json;

use crate::cli::ImportArgs;
use crate::context::Ctx;
use crate::exit::ExitCode;
use crate::output::print_json_success;

/// The built-in import providers (pure file formats). Any other provider falls
/// through to a `clove-import-<provider>` plugin (handled in `main::run_repo`).
pub fn is_builtin(provider: &str) -> bool {
    matches!(provider, "tk" | "beads")
}

/// The flags a built-in import provider accepts, inner-parsed from `rest` so the
/// `KEY=VALUE`-free surface (`clove import tk <src> [--dry-run]`) stays intact
/// while the top-level parser forwards `rest` raw for the plugin fall-through.
#[derive(Debug, Parser)]
#[command(name = "clove import", no_binary_name = true)]
struct ImportBuiltinArgs {
    /// The source (a `.tickets/` directory for tk, an `issues.jsonl` for beads).
    src: Utf8PathBuf,
    /// Plan only: report what would happen without writing any files.
    #[arg(long)]
    dry_run: bool,
}

/// Run a built-in import provider (`tk`/`beads`). `args.provider` is guaranteed a
/// built-in by [`is_builtin`]; `args.rest` is inner-parsed into the source path
/// and `--dry-run`. A parse failure prints clap's error and exits usage-class
/// (mirroring `main`).
pub fn run(ctx: &Ctx, format: OutputFormat, args: ImportArgs) -> Result<ExitCode, CloveError> {
    let parsed = match ImportBuiltinArgs::try_parse_from(args.rest.iter().cloned()) {
        Ok(parsed) => parsed,
        Err(err) => {
            let _ = err.print();
            return Ok(match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => ExitCode::Success,
                _ => ExitCode::Usage,
            });
        }
    };

    let ImportBuiltinArgs { src, dry_run } = parsed;
    let import_ctx = ImportCtx::new(&ctx.store, dry_run).map_err(import_err)?;

    // The two built-ins share the same plan → (drain warnings) → apply/emit flow;
    // only the importer differs. Warnings (tk's title-fallback, beads'
    // comment_count, …) are drained *after* `plan` so they reach both stderr and
    // the JSON envelope's `_meta.warnings`.
    match args.provider.as_str() {
        "tk" => {
            let importer = TkImporter::new(ctx.config.id_prefix.clone(), Utc::now());
            let plan = importer.plan(&src, &import_ctx).map_err(import_err)?;
            let warnings = importer.take_warnings();
            emit(format, dry_run, plan, warnings, |plan| {
                importer.apply(plan, &ctx.store).map_err(import_err)
            })?;
        }
        "beads" => {
            let importer = BeadsImporter::new(ctx.config.id_prefix.clone(), Utc::now());
            let plan = importer.plan(&src, &import_ctx).map_err(import_err)?;
            let warnings = importer.take_warnings();
            emit(format, dry_run, plan, warnings, |plan| {
                importer.apply(plan, &ctx.store).map_err(import_err)
            })?;
        }
        // `is_builtin` gates this call; any other provider is dispatched to a
        // plugin before we get here.
        other => unreachable!("non-built-in import provider `{other}` reached built-in run"),
    }
    Ok(ExitCode::Success)
}

/// Shared tail for both built-ins: surface warnings, then either report the
/// dry-run plan or apply and report the result.
fn emit(
    format: OutputFormat,
    dry_run: bool,
    plan: clove_import::ImportPlan,
    warnings: Vec<String>,
    apply: impl FnOnce(clove_import::ImportPlan) -> Result<clove_import::ImportReport, CloveError>,
) -> Result<(), CloveError> {
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    if dry_run {
        emit_plan(format, &plan, &warnings);
    } else {
        let report = apply(plan)?;
        emit_report(format, &report, &warnings);
    }
    Ok(())
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
