//! `clove sync github <owner/repo> [--dry-run] [--prefer POLICY]` (T-M06).
//!
//! The single GitHub path: one reconciled pass that pulls remote changes *and*
//! pushes local changes, detecting issues changed on both sides since the last
//! sync and resolving them by a [`clove_import::ConflictPolicy`] (default: newest
//! wins, every conflict reported). `--dry-run` plans without touching either side.
//!
//! Like the other GitHub surfaces this is built behind the `github` feature;
//! without it the command is recognized but returns a clean fallback error.

use clove_core::OutputFormat;
use clove_types::CloveError;

use crate::cli::SyncArgs;
use crate::context::Ctx;

pub fn run(ctx: &Ctx, format: OutputFormat, args: SyncArgs) -> Result<(), CloveError> {
    sync_github(ctx, format, args)
}

#[cfg(feature = "github")]
fn sync_github(ctx: &Ctx, format: OutputFormat, args: SyncArgs) -> Result<(), CloveError> {
    use clove_import::ConflictPolicy;
    use serde_json::json;

    use crate::output::print_json_success;

    let policy = match &args.prefer {
        Some(raw) => ConflictPolicy::parse(raw).ok_or_else(|| CloveError::InvalidField {
            field: "prefer".to_owned(),
            reason: format!("expected newer|local|remote|manual, got `{raw}`"),
        })?,
        None => ConflictPolicy::default(),
    };

    let (summary, report) = clove_import::sync_net::sync_github(
        &args.target,
        &ctx.store,
        &ctx.config.id_prefix,
        policy,
        !args.no_comments,
        args.dry_run,
    )
    .map_err(sync_err)?;

    match (format, report) {
        // Applied: emit the action counts (and any conflicts) the run produced.
        (OutputFormat::Json | OutputFormat::Jsonl, Some(report)) => print_json_success(
            json!({
                "pulled_created": report.pulled_created,
                "pulled_updated": report.pulled_updated,
                "pushed_created": report.pushed_created,
                "pushed_updated": report.pushed_updated,
                "comments_pulled": report.comments_pulled,
                "comments_pushed": report.comments_pushed,
                "in_sync": report.in_sync,
                "conflicts": summary.conflicts,
                "remote_missing": summary.remote_missing,
            }),
            json!({ "warnings": [] }),
        ),
        // Dry run: emit the full write-free plan.
        (OutputFormat::Json | OutputFormat::Jsonl, None) => print_json_success(
            serde_json::to_value(&summary).unwrap_or_else(|_| json!({})),
            json!({ "warnings": [] }),
        ),
        (OutputFormat::Human, Some(report)) => {
            println!(
                "synced {}: pulled {} new / {} updated, pushed {} new / {} updated, comments +{}/-{}, {} in sync, {} conflicts",
                args.target,
                report.pulled_created,
                report.pulled_updated,
                report.pushed_created,
                report.pushed_updated,
                report.comments_pulled,
                report.comments_pushed,
                report.in_sync,
                report.conflicts,
            );
            print_conflicts(&summary);
            print_remote_missing(&summary);
        }
        (OutputFormat::Human, None) => {
            println!(
                "dry-run {}: pull {} new / {} updated, push {} new / {} updated, {} in sync, {} conflicts",
                args.target,
                summary.pull_create.len(),
                summary.pull_update.len(),
                summary.push_create.len(),
                summary.push_update.len(),
                summary.in_sync,
                summary.conflicts.len(),
            );
            print_conflicts(&summary);
            print_remote_missing(&summary);
        }
    }
    Ok(())
}

/// Print the per-conflict resolution lines (human output).
#[cfg(feature = "github")]
fn print_conflicts(summary: &clove_import::SyncSummary) {
    for conflict in &summary.conflicts {
        println!(
            "  conflict {} ({})  {} -> {}",
            conflict.external_ref, conflict.clove_id, conflict.title, conflict.resolution
        );
    }
}

/// Warn about local items whose linked GitHub issue was not found.
#[cfg(feature = "github")]
fn print_remote_missing(summary: &clove_import::SyncSummary) {
    for ext in &summary.remote_missing {
        eprintln!("warning: local item links {ext} but the GitHub issue was not found");
    }
}

/// Map a `clove-import` error onto a `CloveError` for exit-code classification.
#[cfg(feature = "github")]
fn sync_err(err: clove_import::ImportError) -> CloveError {
    use clove_import::ImportError;
    match err {
        ImportError::Core(core) => core,
        ImportError::Source { path, message } => CloveError::Io {
            path,
            source: std::io::Error::other(message),
        },
        other => CloveError::Io {
            path: camino::Utf8PathBuf::from("<github>"),
            source: std::io::Error::other(other.to_string()),
        },
    }
}

/// Without the `github` feature, `sync github` is recognized but fails cleanly.
#[cfg(not(feature = "github"))]
fn sync_github(_ctx: &Ctx, _format: OutputFormat, _args: SyncArgs) -> Result<(), CloveError> {
    Err(CloveError::NotYetImplemented {
        feature: "sync github (built without github support)".to_owned(),
    })
}
