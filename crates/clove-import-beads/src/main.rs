//! `clove-import-beads` — the Beads (`issues.jsonl`) import/export plugin
//! (`PLUGIN_SYSTEM.md` §4.2/§6).
//!
//! One binary, two capabilities (§4.2): `clove import beads <src>` drives
//! `clove_import`'s pure `BeadsImporter`, and `clove export beads [--out FILE]`
//! drives the pure `export_beads` writer (the inverse mapping). The host reaches
//! the exporter through the umbrella fallback (`export beads` → `clove-export-beads`
//! miss → this `clove-import-beads`), branching on `$CLOVE_COMMAND`.
//!
//! The import path's stdout/stderr and exit codes are byte-for-byte identical to
//! the former built-in `clove import beads` (dry-run plan envelope, applied report,
//! `_meta.warnings`, human summary). The export path mirrors the built-in
//! `clove export json/jsonl`: a raw beads-native NDJSON dump to stdout (or `--out`),
//! never wrapped in the plugin envelope.
//!
//! Unlike `clove-plugin`'s `run_with_info` harness (which renders *human* output
//! by pretty-printing the data JSON), import needs the host's bespoke human
//! summary and `_meta.warnings`, so this `main` uses the lower-level
//! [`emit_success_with_meta`]/[`emit_error`] directly.

use std::io::Write;
use std::process::ExitCode;

use camino::Utf8PathBuf;
use clap::error::ErrorKind;
use clap::Parser;
use clove_core::OutputFormat;
use clove_import::{export_beads, render, BeadsImporter, ImportCtx, Importer};
use clove_plugin::{
    emit_error, emit_success_with_meta, unsupported_capability, PluginArgs, PluginContext,
    PluginInfo,
};
use clove_types::CloveError;
use serde_json::json;

/// The metadata `clove plugin list` / `--clove-plugin-info` reports, emitted via
/// the shared `clove_plugin::info_requested` so the JSON shape (incl. the §2
/// compat fields) is authored in exactly one place.
const INFO: PluginInfo = PluginInfo {
    name: "clove-import-beads",
    version: env!("CARGO_PKG_VERSION"),
    about: "Import/export Beads issues.jsonl (clove import|export beads)",
    // Bidirectional beads in one binary: the importer plus the inverse exporter,
    // reached via the umbrella fallback for `export beads`.
    provides: &["import:beads", "export:beads"],
};

/// The forwarded-tail args for `import beads` after the cargo-style `import beads`
/// leading echo is stripped. Mirrors the former built-in `ImportBuiltinArgs`.
#[derive(Debug, Parser)]
#[command(name = "clove-import-beads", no_binary_name = true)]
struct Cli {
    /// The source `issues.jsonl` file.
    src: Utf8PathBuf,
    /// Plan only: report what would happen without writing any files.
    #[arg(long)]
    dry_run: bool,
    /// Output format override. Also arrives via `$CLOVE_FORMAT`; accepted here
    /// (per PLUGIN_SYSTEM.md §6.3) so `clove import beads <src> --format json`
    /// works even with the flag after the provider.
    #[arg(long, value_name = "FORMAT", value_parser = parse_format)]
    format: Option<OutputFormat>,
}

/// The forwarded-tail args for `export beads`. Mirrors the built-in
/// `clove export json/jsonl` shape: an optional `--out FILE` sink (else stdout).
#[derive(Debug, Parser)]
#[command(name = "clove-export-beads", no_binary_name = true)]
struct ExportCli {
    /// Write to this file instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<Utf8PathBuf>,
    /// Output format override (accepted for symmetry; the beads dump is always
    /// NDJSON regardless — only the human file-write confirmation honors it).
    #[arg(long, value_name = "FORMAT", value_parser = parse_format)]
    format: Option<OutputFormat>,
}

/// clap value-parser for `--format` (mirrors the host's).
fn parse_format(raw: &str) -> Result<OutputFormat, String> {
    OutputFormat::parse(raw)
        .ok_or_else(|| format!("invalid format `{raw}` (expected human|json|jsonl)"))
}

/// Map a clap parse error to a clove exit code (DESIGN §7.6): `0` for
/// help/version, `1` (Usage) otherwise — never clap's native `2`, which is
/// `NotFound` in clove's table.
fn clap_exit_code(err: &clap::Error) -> u8 {
    match err.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => 0,
        _ => 1,
    }
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

    // 3. Strip the cargo-style leading `<mux> beads` echo (absent when invoked
    //    directly), then branch on the capability the host dispatched us for
    //    (§4.2): `import` drives the importer, `export` the inverse exporter.
    //    Anything else is a structural umbrella miss → exit-2 envelope.
    let tail = PluginArgs::from_argv(&argv, &cx.command, cx.provider.as_deref());
    match cx.command.as_str() {
        "import" => dispatch_import(&cx, &tail),
        "export" => dispatch_export(&cx, &tail),
        _ => {
            let err = unsupported_capability(&INFO, &cx);
            ExitCode::from(emit_error(cx.format, &err, cx.quiet))
        }
    }
}

/// Parse the `import beads` tail and drive the importer.
fn dispatch_import(cx: &PluginContext, tail: &PluginArgs) -> ExitCode {
    let cli = match Cli::try_parse_from(&tail.args) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return ExitCode::from(clap_exit_code(&err));
        }
    };
    // A `--format` after the provider overrides the env-provided one (§6.3).
    let format = cli.format.unwrap_or(cx.format);
    match run(cx, cli, format) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => ExitCode::from(emit_error(format, &err, cx.quiet)),
    }
}

/// Parse the `export beads` tail and dump beads-native NDJSON (stdout or `--out`),
/// mirroring the built-in `clove export json/jsonl`: a raw dump, not an envelope.
fn dispatch_export(cx: &PluginContext, tail: &PluginArgs) -> ExitCode {
    let cli = match ExportCli::try_parse_from(&tail.args) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return ExitCode::from(clap_exit_code(&err));
        }
    };
    let format = cli.format.unwrap_or(cx.format);
    match run_export(cx, cli, format) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => ExitCode::from(emit_error(format, &err, cx.quiet)),
    }
}

/// Drive the beads import and render its result exactly as the host's
/// `cmd/import.rs` did.
fn run(cx: &PluginContext, cli: Cli, format: OutputFormat) -> Result<(), CloveError> {
    let store = cx.open_store();
    let import_ctx = ImportCtx::new(&store, cli.dry_run).map_err(import_err)?;

    let importer = BeadsImporter::new(cx.id_prefix.clone(), chrono::Utc::now());
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

/// Drive the beads export: scan the store and write a beads-native NDJSON dump to
/// stdout (or `--out FILE`), mirroring the built-in `clove export json/jsonl`.
///
/// Like the built-in export, per-file parse failures are dropped (consistent with
/// `ls`/`export`) rather than aborting the dump. The NDJSON is written raw — no
/// plugin envelope — so `clove export beads > issues.jsonl` yields a clean file.
fn run_export(cx: &PluginContext, cli: ExportCli, format: OutputFormat) -> Result<(), CloveError> {
    let store = cx.open_store();
    let (items, _errors) = store.scan()?;

    match &cli.out {
        Some(path) => {
            let mut buf = Vec::new();
            export_beads(&mut buf, &items).map_err(|source| CloveError::Io {
                path: path.clone(),
                source,
            })?;
            std::fs::write(path, &buf).map_err(|source| CloveError::Io {
                path: path.clone(),
                source,
            })?;
            // A short confirmation goes to stderr only (human format), leaving
            // stdout empty so a redirect of the path is uncluttered.
            if matches!(format, OutputFormat::Human) {
                eprintln!("wrote {} items to {path}", items.len());
            }
        }
        None => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            export_beads(&mut handle, &items).map_err(|source| CloveError::Io {
                path: Utf8PathBuf::from("<stdout>"),
                source,
            })?;
            let _ = handle.flush();
        }
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
        let cli = Cli::try_parse_from(["issues.jsonl"]).expect("bare src parses");
        assert_eq!(cli.src, Utf8PathBuf::from("issues.jsonl"));
        assert!(!cli.dry_run);
        assert!(cli.format.is_none());

        let cli = Cli::try_parse_from(["issues.jsonl", "--dry-run", "--format", "json"])
            .expect("flagged form parses");
        assert!(cli.dry_run);
        assert_eq!(cli.format, Some(OutputFormat::Json));
    }

    /// `--format` after the provider is accepted (§6.3), not a hard error.
    #[test]
    fn accepts_format_after_provider() {
        let cli = Cli::try_parse_from(["issues.jsonl", "--format", "jsonl"])
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

    /// The plugin serves both beads directions from one binary.
    #[test]
    fn provides_both_directions() {
        assert!(INFO.provides_capability("import:beads"));
        assert!(INFO.provides_capability("export:beads"));
    }

    /// The `export beads` tail parses the optional `--out`/`--format` (no src).
    #[test]
    fn export_cli_parses_out_and_format() {
        let cli = ExportCli::try_parse_from(Vec::<String>::new()).expect("bare export parses");
        assert!(cli.out.is_none());
        assert!(cli.format.is_none());

        let cli = ExportCli::try_parse_from(["--out", "issues.jsonl", "--format", "json"])
            .expect("flagged export parses");
        assert_eq!(cli.out, Some(Utf8PathBuf::from("issues.jsonl")));
        assert_eq!(cli.format, Some(OutputFormat::Json));
    }

    /// The leading `export beads` echo is stripped just like `import beads`.
    #[test]
    fn strips_leading_export_beads_echo() {
        let host = vec![
            "export".to_owned(),
            "beads".to_owned(),
            "--out".to_owned(),
            "issues.jsonl".to_owned(),
        ];
        let tail = PluginArgs::from_argv(&host, "export", Some("beads"));
        assert_eq!(tail.args, vec!["--out", "issues.jsonl"]);
    }

    /// The leading `import beads` echo is stripped for the host invocation and
    /// left alone for a direct one, matching both call shapes the plugin accepts.
    #[test]
    fn strips_leading_import_beads_echo() {
        let host = vec![
            "import".to_owned(),
            "beads".to_owned(),
            "issues.jsonl".to_owned(),
            "--dry-run".to_owned(),
        ];
        let tail = PluginArgs::from_argv(&host, "import", Some("beads"));
        assert_eq!(tail.args, vec!["issues.jsonl", "--dry-run"]);

        let direct = vec!["issues.jsonl".to_owned()];
        let tail = PluginArgs::from_argv(&direct, "import", Some("beads"));
        assert_eq!(tail.args, vec!["issues.jsonl"]);
    }
}
