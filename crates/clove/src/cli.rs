//! Command-line surface (DESIGN.md §7.1, §7.2). Defines the global flags and
//! the subcommand set. Commands are wired in as their tasks land.

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand, ValueEnum};
use clove_core::OutputFormat;

/// clove — a fast, git-native, dependency-aware work-item tracker.
#[derive(Debug, Parser)]
#[command(name = "clove", version, about, long_about = None)]
pub struct Cli {
    /// Output format.
    #[arg(short = 'f', long, global = true, value_parser = parse_format)]
    pub format: Option<OutputFormat>,

    /// Force a file scan even if an index is present.
    #[arg(long, global = true)]
    pub no_index: bool,

    /// Suppress informational stderr output.
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Terminal color control.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    /// Override `.clove/` discovery with an explicit directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub clove_dir: Option<Utf8PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

/// Terminal color preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

/// The subcommand set. Grows as command tasks (T-CLI*) are implemented.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Print version and schema information.
    Version,
}

/// clap value-parser for [`OutputFormat`].
fn parse_format(raw: &str) -> Result<OutputFormat, String> {
    OutputFormat::parse(raw)
        .ok_or_else(|| format!("invalid format `{raw}` (expected human|json|jsonl)"))
}
