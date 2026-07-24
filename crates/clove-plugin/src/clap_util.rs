//! Shared clap helpers for the `clove-*` multiplexer plugin mains
//! (`PLUGIN_SYSTEM.md` §6.3).
//!
//! Gated behind the `clap` feature so the contract crate stays clap-free for a
//! host or a hand-parsed plugin that does not need it. A plugin that parses argv
//! with clap (almost all do) enables `clove-plugin` with `features = ["clap"]` and
//! reuses these instead of re-authoring the identical mapping in each `main` — the
//! duplication a review flagged across `clove-sync-github` / `clove-import-tk` /
//! `clove-import-beads`.

use clove_core::OutputFormat;

/// clap value-parser for a `--format` argument (mirrors the host's): parses the
/// wire spelling (`human` | `json` | `jsonl`) into an [`OutputFormat`], or returns
/// an error message clap renders on a bad value.
pub fn parse_format(raw: &str) -> Result<OutputFormat, String> {
    OutputFormat::parse(raw)
        .ok_or_else(|| format!("invalid format `{raw}` (expected human|json|jsonl)"))
}

/// Map a clap parse error to a clove exit code (`PLUGIN_SYSTEM.md` §6.3 / DESIGN
/// §7.6): `0` for `--help`/`--version` (a successful display), `1` (Usage)
/// otherwise — never clap's native `2`, which is `NotFound` in clove's exit table.
pub fn clap_exit_code(err: &clap::Error) -> u8 {
    use clap::error::ErrorKind;
    match err.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_format_accepts_wire_spellings_and_rejects_others() {
        assert_eq!(parse_format("json").unwrap(), OutputFormat::Json);
        assert_eq!(parse_format("human").unwrap(), OutputFormat::Human);
        assert!(parse_format("yaml").is_err());
    }

    #[test]
    fn clap_exit_code_maps_help_to_0_and_usage_to_1() {
        use clap::{Arg, Command};
        // A missing required arg is a usage error → exit 1.
        let usage = Command::new("t")
            .arg(Arg::new("x").required(true))
            .try_get_matches_from(["t"])
            .unwrap_err();
        assert_eq!(clap_exit_code(&usage), 1);
        // `--help` is a successful display → exit 0.
        let help = Command::new("t")
            .try_get_matches_from(["t", "--help"])
            .unwrap_err();
        assert_eq!(clap_exit_code(&help), 0);
    }
}
