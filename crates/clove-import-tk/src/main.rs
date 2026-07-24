//! `clove-import-tk` — the tk (`.tickets/`) importer plugin (`PLUGIN_SYSTEM.md`
//! §4.2/§6).
//!
//! It is the extraction of the former built-in `clove import tk`: a
//! `clove-plugin` `main` that materializes the host-exported [`PluginContext`],
//! parses the forwarded `<src> [--dry-run]` args, and drives `clove_import`'s
//! pure `TkImporter` + planning layer. Its stdout/stderr and exit codes are kept
//! byte-for-byte identical to that built-in (dry-run plan envelope, applied
//! report, `_meta.warnings`, and the human summary lines) so scripts, agents, and
//! the existing test suite cannot tell the difference.
//!
//! Unlike `clove-plugin`'s `run_with_info` harness (which renders *human* output
//! by pretty-printing the data JSON), import needs the host's bespoke human
//! summary and `_meta.warnings`, so this `main` uses the lower-level
//! [`emit_success_with_meta`]/[`emit_error`] directly.

use std::process::ExitCode;

use camino::Utf8PathBuf;
use clap::Parser;
use clove_core::OutputFormat;
use clove_import::{render, ImportCtx, Importer, TkImporter};
use clove_plugin::{
    clap_exit_code, emit_error, emit_success_with_meta, parse_format, unsupported_capability,
    PluginArgs, PluginContext, PluginInfo,
};
use clove_types::CloveError;
use serde_json::json;

/// The metadata `clove plugin list` / `--clove-plugin-info` reports, emitted via
/// the shared `clove_plugin::info_requested` so the JSON shape (incl. the §2
/// compat fields) is authored in exactly one place.
const INFO: PluginInfo = PluginInfo {
    name: "clove-import-tk",
    version: env!("CARGO_PKG_VERSION"),
    about: "Import items from a tk .tickets/ directory (clove import tk)",
    provides: &["import:tk"],
};

/// The forwarded-tail args after the cargo-style `import tk` leading echo is
/// stripped. Mirrors the former built-in `ImportBuiltinArgs`.
#[derive(Debug, Parser)]
#[command(name = "clove-import-tk", no_binary_name = true)]
struct Cli {
    /// The source `.tickets/` directory.
    src: Utf8PathBuf,
    /// Plan only: report what would happen without writing any files.
    #[arg(long)]
    dry_run: bool,
    /// Output format override. Also arrives via `$CLOVE_FORMAT`; accepted here
    /// (per PLUGIN_SYSTEM.md §6.3) so `clove import tk <src> --format json`
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

    // 3. tk is import-only. The umbrella fallback for `export tk` would route to
    //    this binary (export → clove-export-tk miss → clove-sync-tk miss →
    //    clove-import-tk), so reject any non-`import` capability up front with the
    //    standard exit-2 envelope rather than silently running the importer (§4.2).
    if cx.command != "import" {
        let err = unsupported_capability(&INFO, &cx);
        return ExitCode::from(emit_error(cx.format, &err, cx.quiet));
    }

    // 4. Strip the cargo-style leading `import tk` echo (absent when invoked
    //    directly as `clove-import-tk <src>`) then parse the tail.
    let tail = PluginArgs::from_argv(&argv, &cx.command, cx.provider.as_deref());
    let cli = match Cli::try_parse_from(&tail.args) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return ExitCode::from(clap_exit_code(&err));
        }
    };

    // 5. Run and render. A `--format` after the provider overrides the
    //    env-provided one (§6.3), for both success and error output.
    let format = cli.format.unwrap_or(cx.format);
    match run(&cx, cli, format) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => ExitCode::from(emit_error(format, &err, cx.quiet)),
    }
}

/// Drive the tk import and render its result exactly as the host's
/// `cmd/import.rs` did.
fn run(cx: &PluginContext, cli: Cli, format: OutputFormat) -> Result<(), CloveError> {
    let store = cx.open_store();
    let import_ctx = ImportCtx::new(&store, cli.dry_run).map_err(import_err)?;

    let importer = TkImporter::new(cx.id_prefix.clone(), chrono::Utc::now());
    let plan = importer.plan(&cli.src, &import_ctx).map_err(import_err)?;
    // Drain warnings *after* `plan` so they reach both stderr and the JSON
    // envelope's `_meta.warnings`.
    let warnings = importer.take_warnings();

    // Warnings go to stderr in every format (mirrors the host's `emit`).
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }

    if cli.dry_run {
        emit_plan(format, &plan, &warnings);
    } else {
        let report = importer.apply(plan, &store).map_err(import_err)?;
        emit_report(format, &report, &warnings);
    }
    Ok(())
}

/// Emit the `--dry-run` plan: the JSON envelope with `_meta.warnings`, or the
/// human summary lines.
fn emit_plan(format: OutputFormat, plan: &clove_import::ImportPlan, warnings: &[String]) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => emit_success_with_meta(
            format,
            render::plan_json(plan),
            json!({ "warnings": warnings }),
        ),
        OutputFormat::Human => println!("{}", render::plan_human(plan)),
    }
}

/// Emit the post-`apply` report: the JSON envelope with `_meta.warnings`, or the
/// human summary line.
fn emit_report(format: OutputFormat, report: &clove_import::ImportReport, warnings: &[String]) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => emit_success_with_meta(
            format,
            render::report_json(report),
            json!({ "warnings": warnings }),
        ),
        OutputFormat::Human => println!("{}", render::report_human(report)),
    }
}

/// Map a `clove-import` error onto a `CloveError` (identical to the host's
/// former `cmd/import.rs::import_err`).
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

#[cfg(test)]
mod tests {
    use super::*;

    /// clap parses the forwarded tail (post-echo-strip) for both the bare-src and
    /// the flagged form.
    #[test]
    fn parses_src_and_flags() {
        let cli = Cli::try_parse_from([".tickets"]).expect("bare src parses");
        assert_eq!(cli.src, Utf8PathBuf::from(".tickets"));
        assert!(!cli.dry_run);
        assert!(cli.format.is_none());

        let cli = Cli::try_parse_from([".tickets", "--dry-run", "--format", "json"])
            .expect("flagged form parses");
        assert!(cli.dry_run);
        assert_eq!(cli.format, Some(OutputFormat::Json));
    }

    /// `--format` after the provider is accepted (§6.3), not a hard error.
    #[test]
    fn accepts_format_after_provider() {
        let cli = Cli::try_parse_from([".tickets", "--format", "jsonl"])
            .expect("--format after src parses");
        assert_eq!(cli.format, Some(OutputFormat::Jsonl));
    }

    /// A missing src is a usage error: clap's native code is 2, mapped to clove's
    /// exit 1 (Usage).
    #[test]
    fn missing_src_maps_to_usage_exit_1() {
        let err = Cli::try_parse_from(Vec::<String>::new()).unwrap_err();
        assert_eq!(clap_exit_code(&err), 1);
    }

    /// `--help` maps to a clean exit 0, not a usage error.
    #[test]
    fn help_maps_to_exit_0() {
        let err = Cli::try_parse_from(["--help"]).unwrap_err();
        assert_eq!(clap_exit_code(&err), 0);
    }

    /// The leading `import tk` echo is stripped for the host invocation and left
    /// alone for a direct one, matching both call shapes the plugin must accept.
    #[test]
    fn strips_leading_import_tk_echo() {
        let host = vec![
            "import".to_owned(),
            "tk".to_owned(),
            ".tickets".to_owned(),
            "--dry-run".to_owned(),
        ];
        let tail = PluginArgs::from_argv(&host, "import", Some("tk"));
        assert_eq!(tail.args, vec![".tickets", "--dry-run"]);

        let direct = vec![".tickets".to_owned()];
        let tail = PluginArgs::from_argv(&direct, "import", Some("tk"));
        assert_eq!(tail.args, vec![".tickets"]);
    }
}
