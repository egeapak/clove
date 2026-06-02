//! Output rendering: the JSON envelope (DESIGN.md §7.3) and human text.
//!
//! JSON (and errors, even in JSON mode) go to **stdout**; human narrative and
//! warnings go to stderr. A JSON consumer therefore always gets valid JSON on
//! stdout regardless of warnings.

use clove_core::{CloveError, OutputFormat};
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
    println!("{envelope}");
}

/// Print one JSONL line: a success envelope whose `data` is a single value.
/// Used by the list commands (`ls`/`query`) once they land.
#[allow(dead_code)]
pub fn print_jsonl_line(data: Value, meta: Value) {
    print_json_success(data, meta);
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
            println!("{envelope}");
        }
        OutputFormat::Human => {
            if !quiet {
                eprintln!("error: {error}");
            }
        }
    }
    exit
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
