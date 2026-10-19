//! Output format for CLI responses (`--format` / `CLOVE_FORMAT`).
//!
//! Lives in `clove-types` (not `clove-core`) so the permissively licensed plugin
//! SDK (`clove-plugin`) can name it without depending on the store crate.

use serde::{Deserialize, Serialize};

/// Output format for CLI responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
    Jsonl,
}

impl OutputFormat {
    /// Parse a format name (used for `CLOVE_FORMAT` and the `--format` flag).
    pub fn parse(value: &str) -> Option<OutputFormat> {
        match value.trim().to_ascii_lowercase().as_str() {
            "human" => Some(OutputFormat::Human),
            "json" => Some(OutputFormat::Json),
            "jsonl" => Some(OutputFormat::Jsonl),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OutputFormat::Human => "human",
            OutputFormat::Json => "json",
            OutputFormat::Jsonl => "jsonl",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_format_parse_roundtrip() {
        for f in [OutputFormat::Human, OutputFormat::Json, OutputFormat::Jsonl] {
            assert_eq!(OutputFormat::parse(f.as_str()), Some(f));
        }
        assert_eq!(OutputFormat::parse("  JSON "), Some(OutputFormat::Json));
        assert_eq!(OutputFormat::parse("nonsense"), None);
    }
}
