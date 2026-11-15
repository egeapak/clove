//! Output rendering: the JSON envelope (DESIGN.md §7.3) and human text.
//!
//! JSON (and errors, even in JSON mode) go to **stdout**; human narrative and
//! warnings go to stderr. A JSON consumer therefore always gets valid JSON on
//! stdout regardless of warnings.

use clove_core::OutputFormat;
use clove_plugin::outln;
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::exit::{classify, ExitCode};

/// The envelope schema version (the `v` field).
pub const ENVELOPE_VERSION: u32 = 1;

/// Resolve the effective output format.
///
/// Precedence (high → low): `--format` flag, `CLOVE_FORMAT` env var, the
/// repository config default (if loaded), then `human`.
pub fn resolve_format(
    flag: Option<OutputFormat>,
    config_default: Option<OutputFormat>,
) -> OutputFormat {
    if let Some(format) = flag {
        return format;
    }
    if let Some(format) = std::env::var("CLOVE_FORMAT")
        .ok()
        .and_then(|value| OutputFormat::parse(&value))
    {
        return format;
    }
    config_default.unwrap_or(OutputFormat::Human)
}

/// Print a successful JSON envelope `{ v, ok: true, data, _meta }` to stdout.
pub fn print_json_success(data: Value, meta: Value) {
    let envelope = json!({
        "v": ENVELOPE_VERSION,
        "ok": true,
        "data": data,
        "_meta": meta,
    });
    outln!("{envelope}");
}

/// Print a list result as one envelope whose `data` is an array.
pub fn print_json_list(items: Vec<Value>, meta: Value) {
    print_json_success(Value::Array(items), meta);
}

/// Print one `{ v, ok, data }` line per element (the `--format jsonl` shape).
/// Each line is independently valid JSON.
pub fn print_jsonl_items(items: &[Value]) {
    for item in items {
        let line = json!({ "v": ENVELOPE_VERSION, "ok": true, "data": item });
        outln!("{line}");
    }
}

/// Print an error envelope `{ v, ok: false, error: { code, message, exit } }`
/// to stdout (JSON mode) or a human message to stderr, and return the exit code.
pub fn emit_error(format: OutputFormat, error: &CloveError, quiet: bool) -> ExitCode {
    let (exit, code) = classify(error);
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let envelope = json!({
                "v": ENVELOPE_VERSION,
                "ok": false,
                "error": {
                    "code": code,
                    "message": error.to_string(),
                    "exit": exit.code(),
                },
            });
            outln!("{envelope}");
        }
        OutputFormat::Human => {
            if !quiet {
                eprintln!("error: {error}");
            }
        }
    }
    exit
}

/// The output format a command line asks for, read from the raw arguments.
///
/// Used only when clap could not parse them — so there is no parsed `--format`
/// to consult — to decide whether the usage error also owes a JSON envelope.
/// Recognizes `--format X`, `--format=X`, `-f X` and `-fX` up to a `--`, then
/// falls back to `CLOVE_FORMAT` and human, like [`resolve_format`].
pub fn requested_format(argv: &[String]) -> OutputFormat {
    let parse = OutputFormat::parse;
    let mut args = argv.iter().skip(1);
    let mut found = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        let value = if arg == "--format" || arg == "-f" {
            args.next().map(String::as_str)
        } else if let Some(value) = arg.strip_prefix("--format=") {
            Some(value)
        } else if arg.starts_with("-f") && !arg.starts_with("--") {
            Some(arg[2..].trim_start_matches('='))
        } else {
            None
        };
        if let Some(format) = value.and_then(parse) {
            found = Some(format);
        }
    }
    resolve_format(found, None)
}

/// The JSON half of a usage error clap already reported on stderr (exit 1): a
/// client that asked for JSON reads stdout, and an empty stdout is not an
/// envelope. Human mode prints nothing more. Returns [`ExitCode::Usage`].
pub fn emit_usage_error(format: OutputFormat, message: &str) -> ExitCode {
    if matches!(format, OutputFormat::Json | OutputFormat::Jsonl) {
        let envelope = json!({
            "v": ENVELOPE_VERSION,
            "ok": false,
            "error": {
                "code": "USAGE_ERROR",
                "message": message.trim_end(),
                "exit": ExitCode::Usage.code(),
            },
        });
        outln!("{envelope}");
    }
    ExitCode::Usage
}

/// Emit an "unknown subcommand" usage error (exit 1, PLUGIN_SYSTEM.md §4.1/§7):
/// a bare `clove <x>` matched no built-in and no `clove-<x>` plugin resolved.
///
/// The message names the binary the host looked for and lists what *is*
/// installed, so the fix (install the plugin) is obvious. JSON/JSONL emit the
/// standard `{ ok:false, error }` envelope with a `UNKNOWN_SUBCOMMAND` code;
/// human mode prints to stderr unless `quiet`. Returns [`ExitCode::Usage`].
pub fn emit_unknown_subcommand(
    format: OutputFormat,
    subcommand: &str,
    binary: &str,
    installed: &[String],
    quiet: bool,
) -> ExitCode {
    let mut message = format!(
        "unknown subcommand `{subcommand}`: no built-in command and no plugin `{binary}` found on the search path"
    );
    if installed.is_empty() {
        message.push_str(" (no plugins installed)");
    } else {
        message.push_str(&format!(" (installed plugins: {})", installed.join(", ")));
    }
    // Point at the command that installs one. This is where a user lands after
    // typing a subcommand that does not exist, so it is the natural place to
    // learn plugins are installable at all.
    message.push_str(&format!(
        "; `clove plugin search {subcommand}` looks for one, `clove plugin install \
         {subcommand}` installs it"
    ));

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let envelope = json!({
                "v": ENVELOPE_VERSION,
                "ok": false,
                "error": {
                    "code": "UNKNOWN_SUBCOMMAND",
                    "message": message,
                    "exit": ExitCode::Usage.code(),
                },
            });
            outln!("{envelope}");
        }
        OutputFormat::Human => {
            if !quiet {
                eprintln!("error: {message}");
            }
        }
    }
    ExitCode::Usage
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_envelope_shape() {
        // We can't easily capture stdout here; assert the classification that
        // feeds the envelope instead.
        let err = CloveError::NotFound {
            id: "proj-00000000".to_owned(),
        };
        let (exit, code) = classify(&err);
        let envelope = json!({
            "v": ENVELOPE_VERSION,
            "ok": false,
            "error": { "code": code, "message": err.to_string(), "exit": exit.code() },
        });
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["error"]["code"], "ITEM_NOT_FOUND");
        assert_eq!(envelope["error"]["exit"], 2);
        assert_eq!(envelope["v"], 1);
    }

    #[test]
    fn requested_format_reads_every_spelling_of_the_flag() {
        let argv = |args: &[&str]| -> Vec<String> {
            std::iter::once("clove")
                .chain(args.iter().copied())
                .map(str::to_owned)
                .collect()
        };
        for args in [
            &["--format", "json", "show"][..],
            &["show", "--format=json"],
            &["-f", "json", "show"],
            &["-fjson", "show"],
        ] {
            assert_eq!(
                requested_format(&argv(args)),
                OutputFormat::Json,
                "{args:?}"
            );
        }
        // After `--` the tokens are positionals, not flags.
        if std::env::var("CLOVE_FORMAT").is_err() {
            assert_eq!(
                requested_format(&argv(&["new", "--", "--format", "json"])),
                OutputFormat::Human
            );
            assert_eq!(requested_format(&argv(&["show"])), OutputFormat::Human);
        }
    }

    #[test]
    fn resolve_format_prefers_flag() {
        assert_eq!(
            resolve_format(Some(OutputFormat::Json), Some(OutputFormat::Human)),
            OutputFormat::Json
        );
    }

    #[test]
    fn resolve_format_falls_back_to_config_then_human() {
        // With no flag and (assuming) no CLOVE_FORMAT in the test env, config wins.
        if std::env::var("CLOVE_FORMAT").is_err() {
            assert_eq!(
                resolve_format(None, Some(OutputFormat::Jsonl)),
                OutputFormat::Jsonl
            );
            assert_eq!(resolve_format(None, None), OutputFormat::Human);
        }
    }
}
