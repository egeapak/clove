//! The output envelope (`PLUGIN_SYSTEM.md` §6.3, `DESIGN.md` §7.3) — the
//! plugin-side copy of the host's `crates/clove/src/output.rs`.
//!
//! A plugin must emit byte-identical envelopes to a built-in so scripts and
//! agents can't tell the difference: `{ v, ok, data, _meta }` on success,
//! `{ v, ok: false, error: { code, message, exit } }` on failure — both on
//! **stdout**. The `(code, exit)` pair comes from [`clove_types::error_code`],
//! the single classification shared with the host and the web API. Human
//! narrative goes to stderr and is suppressed by `CLOVE_QUIET`.

use clove_core::OutputFormat;
use clove_types::CloveError;
use serde_json::{json, Value};

/// The envelope schema version (the `v` field). Matches the host's
/// `output::ENVELOPE_VERSION`.
pub const ENVELOPE_VERSION: u32 = 1;

/// Print a successful envelope for `data`, honoring `format`.
///
/// JSON / JSONL emit `{ v, ok: true, data, _meta }` on stdout (with an empty
/// `_meta` object — a plugin has no host-level meta to add). Human mode prints a
/// concise pretty-printed rendering of `data` on stdout.
///
/// A thin wrapper over [`emit_success_with_meta`] with an empty `_meta`.
pub fn emit_success(format: OutputFormat, data: Value) {
    emit_success_with_meta(format, data, json!({}));
}

/// Print a successful envelope for `data` with an explicit `_meta`, honoring
/// `format`.
///
/// JSON / JSONL emit `{ v, ok: true, data, _meta }` on stdout with the given
/// `meta` (e.g. `{ "warnings": [...] }` for the import plugins, matching the
/// host's `output::print_json_success`). Human mode prints a concise
/// pretty-printed rendering of `data` on stdout (the meta is a machine-envelope
/// concern; human warnings go to stderr).
pub fn emit_success_with_meta(format: OutputFormat, data: Value, meta: Value) {
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let envelope = json!({
                "v": ENVELOPE_VERSION,
                "ok": true,
                "data": data,
                "_meta": meta,
            });
            println!("{envelope}");
        }
        OutputFormat::Human => {
            println!(
                "{}",
                serde_json::to_string_pretty(&data).unwrap_or_default()
            );
        }
    }
}

/// Print an error envelope for `err`, honoring `format`, and return the numeric
/// exit code (the `DESIGN.md` §7.6 table) the process should exit with.
///
/// JSON / JSONL emit `{ v, ok: false, error: { code, message, exit } }` on
/// stdout. Human mode prints `error: <message>` to stderr unless `quiet`.
pub fn emit_error(format: OutputFormat, err: &CloveError, quiet: bool) -> u8 {
    let (code, exit) = clove_types::error_code(err);
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let envelope = json!({
                "v": ENVELOPE_VERSION,
                "ok": false,
                "error": {
                    "code": code,
                    "message": err.to_string(),
                    "exit": exit,
                },
            });
            println!("{envelope}");
        }
        OutputFormat::Human => {
            if !quiet {
                eprintln!("error: {err}");
            }
        }
    }
    exit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_envelope_classification() {
        // stdout is not easily captured here; assert the classification that
        // feeds the envelope (and the returned exit code) instead.
        let err = CloveError::NotFound {
            id: "clove-00000000".to_owned(),
        };
        let (code, exit) = clove_types::error_code(&err);
        let envelope = json!({
            "v": ENVELOPE_VERSION,
            "ok": false,
            "error": { "code": code, "message": err.to_string(), "exit": exit },
        });
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["error"]["code"], "ITEM_NOT_FOUND");
        assert_eq!(envelope["error"]["exit"], 2);
        assert_eq!(envelope["v"], 1);
    }

    #[test]
    fn emit_error_returns_exit_code() {
        let err = CloveError::InvalidField {
            field: "priority".to_owned(),
            reason: "out of range".to_owned(),
        };
        assert_eq!(emit_error(OutputFormat::Json, &err, false), 4);
        // Human + quiet suppresses the stderr line but still classifies.
        assert_eq!(emit_error(OutputFormat::Human, &err, true), 4);
    }
}
