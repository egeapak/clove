//! `clove-sync-github` — the first-party GitHub sync plugin (`PLUGIN_SYSTEM.md`
//! §6/§8).
//!
//! It is the fat-plugin extraction of `clove sync github`: a `clove-plugin`
//! `main` that materializes the host-exported [`PluginContext`], parses the
//! forwarded `OWNER/REPO [--dry-run] [--prefer POLICY] [--no-comments]` args, and
//! drives `clove_import::sync_net::sync_github` — the *same* entry point the
//! in-process `cmd/sync.rs` used. Its stdout/stderr and exit codes are kept
//! byte-for-byte identical to that built-in so scripts, agents, and the existing
//! mock suite cannot tell the difference (only the JSON `_meta` differs: an empty
//! object here, as every plugin emits, vs the host's `{ "warnings": [] }` — no
//! consumer keys on it).
//!
//! Unlike `clove-plugin`'s [`run_with_info`](clove_plugin::run_with_info) harness
//! (which renders *human* output by pretty-printing the data JSON), sync needs the
//! host's bespoke human summary + conflict/remote-missing notes, so this `main`
//! uses the lower-level [`emit_success`]/[`emit_error`] directly.

use std::process::ExitCode;

use clap::Parser;
use clove_core::OutputFormat;
use clove_plugin::{emit_error, emit_success, PluginArgs, PluginContext};
use clove_types::CloveError;
use serde_json::json;

/// The `--clove-plugin-info` metadata probe token (§7).
const INFO_FLAG: &str = "--clove-plugin-info";

/// The plugin binary name (also the `--clove-plugin-info` `name`).
const NAME: &str = "clove-sync-github";

/// The one-line description surfaced by `clove plugin list`.
const ABOUT: &str = "Two-way GitHub Issues sync (clove sync github)";

/// The forwarded-tail args after the cargo-style `sync github` leading echo is
/// stripped. Mirrors the host's `SyncArgs` (minus the `tracker` selector, which
/// the dispatch already consumed by choosing this plugin).
#[derive(Debug, Parser)]
#[command(name = "clove-sync-github", no_binary_name = true)]
struct Cli {
    /// The `owner/repo` to sync with.
    #[arg(value_name = "OWNER/REPO")]
    target: String,
    /// Plan only: report what would happen on both sides without writing anything.
    #[arg(long)]
    dry_run: bool,
    /// Conflict policy for issues changed on both sides since the last sync:
    /// `newer` (default), `local`, `remote`, or `manual`.
    #[arg(long, value_name = "POLICY")]
    prefer: Option<String>,
    /// Skip syncing issue comments (faster: avoids one API call per issue).
    #[arg(long)]
    no_comments: bool,
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    // 1. Answer the §7 metadata probe *before* env materialization so `clove
    //    plugin list` can describe the plugin with no repo context.
    if argv.iter().any(|arg| arg == INFO_FLAG) {
        let info = json!({
            "name": NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "about": ABOUT,
            "provides": ["sync:github"],
        });
        println!("{info}");
        return ExitCode::SUCCESS;
    }

    // 2. Materialize the typed §6.2 context. On failure the typed context is
    //    unavailable, so read the format/quiet hints directly (§6.5) and render
    //    the standard error envelope with the validation exit code (4).
    let cx = match PluginContext::from_env() {
        Ok(cx) => cx,
        Err(env_err) => {
            let format = std::env::var("CLOVE_FORMAT")
                .ok()
                .and_then(|value| OutputFormat::parse(&value))
                .unwrap_or_default();
            let quiet = std::env::var("CLOVE_QUIET").is_ok_and(|value| value == "1");
            let err: CloveError = env_err.into();
            return ExitCode::from(emit_error(format, &err, quiet));
        }
    };

    // 3. Strip the cargo-style leading `sync github` echo (absent when invoked
    //    directly as `clove-sync-github OWNER/REPO`) then parse the tail.
    let tail = PluginArgs::from_argv(&argv, &cx.command, cx.provider.as_deref());
    let cli = match Cli::try_parse_from(&tail.args) {
        Ok(cli) => cli,
        Err(err) => {
            // clap already renders usage/help; propagate its exit code (0 for
            // --help/--version, 2 for a usage error).
            let _ = err.print();
            return ExitCode::from(err.exit_code().clamp(0, 255) as u8);
        }
    };

    // 4. Run and render. On error, classify through the shared error_code table.
    match run(&cx, cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => ExitCode::from(emit_error(cx.format, &err, cx.quiet)),
    }
}

/// Drive the sync and render its result exactly as the host's `cmd/sync.rs` did.
///
/// Emits the success envelope / human summary itself and returns `Ok(())`; any
/// `Err` (a bad `--prefer` value or a sync failure) is rendered by the caller.
fn run(cx: &PluginContext, cli: Cli) -> Result<(), CloveError> {
    use clove_import::ConflictPolicy;

    let policy = match &cli.prefer {
        Some(raw) => ConflictPolicy::parse(raw).ok_or_else(|| CloveError::InvalidField {
            field: "prefer".to_owned(),
            reason: format!("expected newer|local|remote|manual, got `{raw}`"),
        })?,
        None => ConflictPolicy::default(),
    };

    let (summary, report) = clove_import::sync_net::sync_github(
        &cli.target,
        &cx.open_store(),
        &cx.id_prefix,
        policy,
        !cli.no_comments,
        cli.dry_run,
    )
    .map_err(sync_err)?;

    match (cx.format, report) {
        // Applied: emit the action counts (and any conflicts) the run produced.
        (OutputFormat::Json | OutputFormat::Jsonl, Some(report)) => emit_success(
            cx.format,
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
                "foreign": summary.foreign,
            }),
        ),
        // Dry run: emit the full write-free plan.
        (OutputFormat::Json | OutputFormat::Jsonl, None) => emit_success(
            cx.format,
            serde_json::to_value(&summary).unwrap_or_else(|_| json!({})),
        ),
        (OutputFormat::Human, Some(report)) => {
            println!(
                "synced {}: pulled {} new / {} updated, pushed {} new / {} updated, comments +{}/-{}, {} in sync, {} conflicts",
                cli.target,
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
                cli.target,
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
fn print_conflicts(summary: &clove_import::SyncSummary) {
    for conflict in &summary.conflicts {
        println!(
            "  conflict {} ({})  {} -> {}",
            conflict.external_ref, conflict.clove_id, conflict.title, conflict.resolution
        );
    }
}

/// Warn about local items whose linked GitHub issue was not found, and note items
/// linked to a different external system (skipped by a GitHub sync).
fn print_remote_missing(summary: &clove_import::SyncSummary) {
    for ext in &summary.remote_missing {
        eprintln!("warning: local item links {ext} but the GitHub issue was not found");
    }
    for ext in &summary.foreign {
        eprintln!("note: local item links {ext} (not a GitHub link) — skipped");
    }
}

/// Map a `clove-import` error onto a `CloveError` for exit-code classification
/// (identical to the host's `cmd/sync.rs::sync_err`).
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

#[cfg(test)]
mod tests {
    use super::*;
    use clove_import::ConflictPolicy;

    /// clap parses the forwarded tail (post-echo-strip) for both the bare-target
    /// and the fully-flagged form.
    #[test]
    fn parses_target_and_flags() {
        let cli = Cli::try_parse_from(["egeapak/clove"]).expect("bare target parses");
        assert_eq!(cli.target, "egeapak/clove");
        assert!(!cli.dry_run);
        assert!(cli.prefer.is_none());
        assert!(!cli.no_comments);

        let cli = Cli::try_parse_from([
            "egeapak/clove",
            "--dry-run",
            "--prefer",
            "remote",
            "--no-comments",
        ])
        .expect("flagged form parses");
        assert_eq!(cli.target, "egeapak/clove");
        assert!(cli.dry_run);
        assert_eq!(cli.prefer.as_deref(), Some("remote"));
        assert!(cli.no_comments);
    }

    /// A missing target is a clap usage error (exit 2), never a panic.
    #[test]
    fn missing_target_is_usage_error() {
        let err = Cli::try_parse_from(Vec::<String>::new()).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    /// The leading `sync github` echo is stripped for the host invocation and left
    /// alone for a direct one, matching both call shapes the plugin must accept.
    #[test]
    fn strips_leading_sync_github_echo() {
        let host = vec![
            "sync".to_owned(),
            "github".to_owned(),
            "egeapak/clove".to_owned(),
            "--dry-run".to_owned(),
        ];
        let tail = PluginArgs::from_argv(&host, "sync", Some("github"));
        assert_eq!(tail.args, vec!["egeapak/clove", "--dry-run"]);

        let direct = vec!["egeapak/clove".to_owned()];
        let tail = PluginArgs::from_argv(&direct, "sync", Some("github"));
        assert_eq!(tail.args, vec!["egeapak/clove"]);
    }

    /// A bad `--prefer` value would map to InvalidField (validation class, exit 4),
    /// exactly as the host did.
    #[test]
    fn bad_prefer_is_validation_class() {
        assert!(ConflictPolicy::parse("bogus").is_none());
        let err = CloveError::InvalidField {
            field: "prefer".to_owned(),
            reason: "expected newer|local|remote|manual, got `bogus`".to_owned(),
        };
        assert_eq!(clove_types::error_code(&err).1, 4);
    }
}
