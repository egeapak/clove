//! clove CLI entry point.
//!
//! Thin shell over `clove-core` (and, from M1, `clove-index`). JSON everywhere;
//! exit codes per DESIGN.md §7.6. The parser lives in [`cli`], output rendering
//! in [`output`], the exit-code table in [`exit`], and each subcommand under
//! [`cmd`].

mod cli;
mod cmd;
mod exit;
mod output;

use clap::error::ErrorKind;
use clap::Parser;

use cli::{Cli, Commands};
use exit::ExitCode;
use output::{emit_error, resolve_format};

fn main() -> std::process::ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // `--help` / `--version` are not failures.
            let _ = err.print();
            return match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    std::process::ExitCode::SUCCESS
                }
                _ => ExitCode::Usage.process(),
            };
        }
    };

    // No repo config is loaded yet (commands that need it will load it and pass
    // its default into `resolve_format`).
    let format = resolve_format(cli.format, None);

    let result = match cli.command {
        Commands::Version => cmd::version::run(format),
    };

    match result {
        Ok(()) => ExitCode::Success.process(),
        Err(error) => emit_error(format, &error, cli.quiet).process(),
    }
}
