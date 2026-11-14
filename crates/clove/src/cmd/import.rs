//! `clove import <json|jsonl> <file> [--dry-run] [--overwrite]` — clove's native
//! **restore**, the built-in inverse of `export json` / `export jsonl`.
//!
//! Symmetric with [`crate::cmd::export`]: `json`/`jsonl` are the only built-in
//! providers (they re-read clove's own export verbatim, preserving ids); any
//! other provider (`tk`, `beads`, …) falls through to a
//! `clove-import-<provider>` plugin in `main::run_repo`. The restore engine
//! itself (parse/plan/apply) lives in `clove_import::restore`; this module is the
//! thin CLI shell: inner-parse `rest`, read the file, run the engine, and emit the
//! standard envelope with any warnings threaded into `_meta.warnings`.

use camino::{Utf8Path, Utf8PathBuf};
use clap::error::ErrorKind;
use clap::Parser;
use clove_core::OutputFormat;
use clove_import::{
    apply_restore, parse_export_json, parse_export_jsonl, plan_restore, ImportError, RestorePlan,
    RestoreReport,
};
use clove_plugin::outln;
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::cli::ImportArgs;
use crate::context::Ctx;
use crate::exit::ExitCode;
use crate::output::print_json_success;

/// The built-in import providers (clove's native restore formats). Any other
/// provider falls through to a `clove-import-<provider>` plugin (handled in
/// `main::run_repo`).
pub fn is_builtin(provider: &str) -> bool {
    matches!(provider, "json" | "jsonl")
}

// Inner-parsed from `rest` (the format itself comes from `provider`, not a
// positional) so the top-level router can forward `rest` raw for the plugin
// fall-through, mirroring `cmd::export`. The `about` is user-facing (shown on
// `--help`); this comment is not.
#[derive(Debug, Parser)]
#[command(
    name = "clove import <json|jsonl>",
    no_binary_name = true,
    about = "Restore items from a clove json/jsonl export (preserving ids)"
)]
struct ImportBuiltinArgs {
    /// The export file to restore from.
    #[arg(value_name = "FILE")]
    src: Utf8PathBuf,
    /// Show the restore plan without writing anything.
    #[arg(long)]
    dry_run: bool,
    /// Replace items whose id already exists (default: skip them).
    #[arg(long)]
    overwrite: bool,
}

/// Run a built-in import provider (`json`/`jsonl`). `args.provider` is guaranteed
/// a built-in by [`is_builtin`]; `args.rest` is inner-parsed into `src` +
/// `--dry-run`/`--overwrite`. A parse failure prints clap's error and exits
/// usage-class (mirroring `cmd::export::run`).
pub fn run(ctx: &Ctx, format: OutputFormat, args: ImportArgs) -> Result<ExitCode, CloveError> {
    let parsed = match ImportBuiltinArgs::try_parse_from(args.rest.iter().cloned()) {
        Ok(parsed) => parsed,
        Err(err) => {
            let _ = err.print();
            // A global flag placed *after* the provider lands in `rest` and reads
            // as an unknown argument here; clap's default `-- --format` tip is
            // wrong for that case, so point at the real fix.
            if err.kind() == ErrorKind::UnknownArgument {
                eprintln!(
                    "\nnote: clove global flags (--format, --color, --quiet, …) must come \
                     before the provider, e.g. `clove import --format json {} …`",
                    args.provider
                );
            }
            return Ok(match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => ExitCode::Success,
                _ => crate::output::emit_usage_error(format, &err.to_string()),
            });
        }
    };

    // Read the export file. A missing/unreadable file is an I/O error (exit 5).
    let text = std::fs::read_to_string(&parsed.src).map_err(|source| CloveError::Io {
        path: parsed.src.clone(),
        source,
    })?;

    // Parse per the provider format, collecting any per-item/per-line warnings.
    let (items, warnings) = match args.provider.as_str() {
        "json" => parse_export_json(&text),
        "jsonl" => parse_export_jsonl(&text),
        // `is_builtin` gates this call; any other provider is dispatched to a
        // plugin before we get here.
        other => unreachable!("non-built-in import provider `{other}` reached built-in run"),
    }
    .map_err(|err| import_err(err, &parsed.src))?;

    // Surface warnings on stderr (human channel) regardless of format, and also
    // into the JSON envelope's `_meta.warnings`.
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    let meta = json!({ "warnings": warnings });

    // Validate every item that would be written *before* anything is: the
    // dry run and the real run then agree, and a bad item cannot stop a real
    // run half-way with the earlier items already on disk.
    validate_writable(&items, ctx, parsed.overwrite)?;

    if parsed.dry_run {
        let plan = plan_restore(&items, &ctx.store, parsed.overwrite)
            .map_err(|err| import_err(err, &parsed.src))?;
        emit_plan(format, &plan, meta);
    } else {
        let report = apply_restore(&items, &ctx.store, parsed.overwrite)
            .map_err(|err| import_err(err, &parsed.src))?;
        emit_report(format, &report, meta);
    }

    Ok(ExitCode::Success)
}

/// Check every item the restore would write against the same rules
/// `ItemStore::restore_item` applies (title, assignee, frontmatter validation).
/// Items that would be skipped (existing id, no `--overwrite`) are not checked,
/// as the apply never validates them either. Every failure is reported at once,
/// named by item id.
fn validate_writable(
    items: &[clove_types::Item],
    ctx: &Ctx,
    overwrite: bool,
) -> Result<(), CloveError> {
    let mut problems = Vec::new();
    for item in items {
        let fm = &item.frontmatter;
        if !overwrite && ctx.store.exists(&fm.id) {
            continue;
        }
        let mut reasons: Vec<String> = Vec::new();
        if let Err(error) = clove_types::fields::parse_title(&fm.title) {
            reasons.push(error.to_string());
        }
        if let Err(error) = clove_types::fields::parse_assignee(fm.assignee.clone()) {
            reasons.push(error.to_string());
        }
        reasons.extend(
            clove_types::validate_item(fm)
                .iter()
                .map(ToString::to_string),
        );
        if !reasons.is_empty() {
            problems.push(format!("{}: {}", fm.id, reasons.join(", ")));
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    Err(CloveError::InvalidField {
        field: "import".to_owned(),
        reason: format!(
            "{} item(s) are invalid, nothing was imported: {}",
            problems.len(),
            problems.join("; ")
        ),
    })
}

/// Emit a `--dry-run` [`RestorePlan`]: the `{would_create, would_skip,
/// would_overwrite}` payload as JSON, or a counted human summary on stdout.
fn emit_plan(format: OutputFormat, plan: &RestorePlan, meta: Value) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let data = serde_json::to_value(plan).unwrap_or(Value::Null);
            print_json_success(data, meta);
        }
        OutputFormat::Human => {
            outln!(
                "dry run: would create {}, skip {}, overwrite {}",
                plan.would_create.len(),
                plan.would_skip.len(),
                plan.would_overwrite.len(),
            );
        }
    }
}

/// Emit an applied [`RestoreReport`]: the `{created, skipped, overwritten}`
/// payload as JSON, or a counted human summary on stdout.
fn emit_report(format: OutputFormat, report: &RestoreReport, meta: Value) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let data = serde_json::to_value(report).unwrap_or(Value::Null);
            print_json_success(data, meta);
        }
        OutputFormat::Human => {
            outln!(
                "restored: {} created, {} skipped, {} overwritten",
                report.created,
                report.skipped,
                report.overwritten,
            );
        }
    }
}

/// Map an [`ImportError`] to a [`CloveError`] so the CLI's exit-code table
/// applies: an export produced by a newer clove is a validation error (exit 4)
/// with an upgrade hint; a `clove-core` failure passes through with its own
/// class; source/record parse failures are I/O (exit 5) scoped to `src`.
fn import_err(err: ImportError, src: &Utf8Path) -> CloveError {
    match err {
        ImportError::Incompatible { message } => CloveError::InvalidField {
            field: "import".to_owned(),
            reason: message,
        },
        ImportError::Core(inner) => inner,
        ImportError::Source { path, message } => CloveError::Io {
            path,
            source: std::io::Error::other(message),
        },
        ImportError::Record { message } => CloveError::Io {
            path: src.to_owned(),
            source: std::io::Error::other(message),
        },
        // `ImportError` is `#[non_exhaustive]`; any future variant is treated as
        // an I/O-class failure scoped to the source file.
        other => CloveError::Io {
            path: src.to_owned(),
            source: std::io::Error::other(other.to_string()),
        },
    }
}
