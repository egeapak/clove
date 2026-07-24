//! `clove-sync-github` — the first-party GitHub sync plugin (`PLUGIN_SYSTEM.md`
//! §6/§8).
//!
//! It is the fat-plugin extraction of `clove sync github`: a `clove-plugin`
//! `main` that materializes the host-exported [`PluginContext`], parses the
//! forwarded `OWNER/REPO [--dry-run] [--prefer POLICY] [--no-comments]` args, and
//! drives `clove_import::sync_net::sync_github` — the *same* entry point the
//! in-process `cmd/sync.rs` used. Its `sync github` stdout/stderr and exit codes
//! are kept byte-for-byte identical to that built-in so scripts, agents, and the
//! existing mock suite cannot tell the difference (only the JSON `_meta` differs:
//! an empty object here, as every plugin emits, vs the host's `{ "warnings": [] }`
//! — no consumer keys on it).
//!
//! One binary, three capabilities (`PLUGIN_SYSTEM.md` §4.2): the same reconcile
//! planner serves the full two-way `sync github` (`Direction::Both`) and the
//! one-way views `import github` (`PullOnly`) and `export github` (`PushOnly`),
//! reached via the host's umbrella fallback. `main` picks the [`Direction`] from
//! `$CLOVE_COMMAND`.
//!
//! Unlike `clove-plugin`'s [`run_with_info`](clove_plugin::run_with_info) harness
//! (which renders *human* output by pretty-printing the data JSON), sync needs the
//! host's bespoke human summary + conflict/remote-missing notes, so this `main`
//! uses the lower-level [`emit_success`]/[`emit_error`] directly.

use std::process::ExitCode;

use clap::Parser;
use clove_core::OutputFormat;
use clove_plugin::{
    clap_exit_code, emit_error, emit_success, parse_format, PluginArgs, PluginContext, PluginInfo,
};
use clove_types::CloveError;
use serde_json::json;

/// The metadata `clove plugin list` / `--clove-plugin-info` reports. Emitted via
/// the shared `clove_plugin::info_requested` so the JSON shape (incl. the §2
/// compat fields) is authored in exactly one place — this plugin hand-rolls only
/// its *human* rendering, never the info blob.
const INFO: PluginInfo = PluginInfo {
    name: "clove-sync-github",
    version: env!("CARGO_PKG_VERSION"),
    about: "GitHub Issues sync/import/export (clove sync|import|export github)",
    // One binary, three capabilities (PLUGIN_SYSTEM.md §4.2): the full two-way
    // `sync github`, plus the one-way views `import github` (pull) and
    // `export github` (push), reached via the umbrella fallback.
    provides: &["sync:github", "import:github", "export:github"],
};

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
    /// Output format override. Also arrives via `$CLOVE_FORMAT`; accepted here
    /// (per PLUGIN_SYSTEM.md §6.3) so `clove sync github <repo> --format json`
    /// works even with the flag after the provider.
    #[arg(long, value_name = "FORMAT", value_parser = parse_format)]
    format: Option<OutputFormat>,
}

fn main() -> ExitCode {
    // 1. Answer the §7 metadata probe *before* env materialization so `clove
    //    plugin list` can describe the plugin with no repo context.
    if clove_plugin::info_requested(&INFO) {
        return ExitCode::SUCCESS;
    }

    let argv: Vec<String> = std::env::args().skip(1).collect();

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

    // 3. Pick the reconcile direction from the capability the host dispatched us
    //    for (§4.2): `sync` is two-way, `import` pulls, `export` pushes. Anything
    //    else means a structural umbrella dispatch reached us for a capability we
    //    do not implement — reject it with the standard exit-2 envelope *before*
    //    parsing, so an unsupported dispatch fails as UNSUPPORTED_CAPABILITY rather
    //    than a clap usage error over the (irrelevant) missing target. This mirrors
    //    the guard-before-parse order in clove-import-{tk,beads}.
    let direction = match cx.command.as_str() {
        "sync" => clove_import::Direction::Both,
        "import" => clove_import::Direction::PullOnly,
        "export" => clove_import::Direction::PushOnly,
        _ => {
            let err = clove_plugin::unsupported_capability(&INFO, &cx);
            return ExitCode::from(emit_error(cx.format, &err, cx.quiet));
        }
    };

    // 4. Strip the cargo-style leading `sync github` echo (absent when invoked
    //    directly as `clove-sync-github OWNER/REPO`) then parse the tail.
    let tail = PluginArgs::from_argv(&argv, &cx.command, cx.provider.as_deref());
    let cli = match Cli::try_parse_from(&tail.args) {
        Ok(cli) => cli,
        Err(err) => {
            // clap already renders usage/help; map its kind to clove's exit
            // table (0 for --help/--version, 1 for a usage error — not clap's
            // native 2, which means NotFound in clove).
            let _ = err.print();
            return ExitCode::from(clap_exit_code(&err));
        }
    };

    // 5. Run and render. A `--format` after the provider overrides the
    //    env-provided one (§6.3), for both success and error output.
    let format = cli.format.unwrap_or(cx.format);
    match run(&cx, cli, format, direction) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => ExitCode::from(emit_error(format, &err, cx.quiet)),
    }
}

/// Drive the reconcile in `direction` and render its result exactly as the host's
/// `cmd/sync.rs` did (`sync`), or as a one-way pull/push (`import`/`export`).
///
/// Emits the success envelope / human summary itself and returns `Ok(())`; any
/// `Err` (a bad `--prefer` value or a sync failure) is rendered by the caller.
fn run(
    cx: &PluginContext,
    cli: Cli,
    format: OutputFormat,
    direction: clove_import::Direction,
) -> Result<(), CloveError> {
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
        direction,
    )
    .map_err(sync_err)?;

    match (format, report) {
        // Applied: emit the action counts (and any conflicts) the run produced.
        (OutputFormat::Json | OutputFormat::Jsonl, Some(report)) => emit_success(
            format,
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
            format,
            serde_json::to_value(&summary).unwrap_or_else(|_| json!({})),
        ),
        (OutputFormat::Human, Some(report)) => {
            println!(
                "{}: pulled {} new / {} updated, pushed {} new / {} updated, comments +{}/-{}, {} in sync, {} conflicts",
                applied_label(direction, &cli.target),
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
                "{}: pull {} new / {} updated, push {} new / {} updated, {} in sync, {} conflicts",
                dry_run_label(direction, &cli.target),
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

/// The human summary prefix for an applied run. `Both` reproduces the former
/// built-in's `synced <target>` byte-for-byte (the mock suite pins it); the
/// one-way views read `imported`/`exported`.
fn applied_label(direction: clove_import::Direction, target: &str) -> String {
    match direction {
        clove_import::Direction::Both => format!("synced {target}"),
        clove_import::Direction::PullOnly => format!("imported {target}"),
        clove_import::Direction::PushOnly => format!("exported {target}"),
    }
}

/// The human summary prefix for a `--dry-run`. `Both` reproduces the former
/// built-in's `dry-run <target>` byte-for-byte; the one-way views name the
/// direction (`dry-run import`/`dry-run export`).
fn dry_run_label(direction: clove_import::Direction, target: &str) -> String {
    match direction {
        clove_import::Direction::Both => format!("dry-run {target}"),
        clove_import::Direction::PullOnly => format!("dry-run import {target}"),
        clove_import::Direction::PushOnly => format!("dry-run export {target}"),
    }
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

    /// A missing target is a usage error. clap's native code is 2, but we map it
    /// to clove's exit 1 (Usage) — clap's 2 is `NotFound` in clove's table.
    #[test]
    fn missing_target_maps_to_usage_exit_1() {
        let err = Cli::try_parse_from(Vec::<String>::new()).unwrap_err();
        assert_eq!(err.exit_code(), 2, "clap's native code");
        assert_eq!(clap_exit_code(&err), 1, "mapped to clove Usage");
    }

    /// `--help` maps to a clean exit 0, not a usage error.
    #[test]
    fn help_maps_to_exit_0() {
        let err = Cli::try_parse_from(["--help"]).unwrap_err();
        assert_eq!(clap_exit_code(&err), 0);
    }

    /// `--format` after the provider is accepted (§6.3), not a hard error.
    #[test]
    fn accepts_format_after_provider() {
        let cli = Cli::try_parse_from(["egeapak/clove", "--format", "json"])
            .expect("--format after target parses");
        assert_eq!(cli.format, Some(OutputFormat::Json));
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

    /// `Both` reproduces the former built-in's human prefixes byte-for-byte (the
    /// mock suite pins `sync github`); the one-way views read differently.
    #[test]
    fn direction_labels() {
        use clove_import::Direction;
        assert_eq!(applied_label(Direction::Both, "o/r"), "synced o/r");
        assert_eq!(applied_label(Direction::PullOnly, "o/r"), "imported o/r");
        assert_eq!(applied_label(Direction::PushOnly, "o/r"), "exported o/r");
        assert_eq!(dry_run_label(Direction::Both, "o/r"), "dry-run o/r");
        assert_eq!(
            dry_run_label(Direction::PullOnly, "o/r"),
            "dry-run import o/r"
        );
        assert_eq!(
            dry_run_label(Direction::PushOnly, "o/r"),
            "dry-run export o/r"
        );
    }

    /// The plugin advertises all three capabilities it serves from one binary.
    #[test]
    fn provides_all_three_capabilities() {
        assert!(INFO.provides_capability("sync:github"));
        assert!(INFO.provides_capability("import:github"));
        assert!(INFO.provides_capability("export:github"));
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
