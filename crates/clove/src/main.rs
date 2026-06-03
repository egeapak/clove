//! clove CLI entry point.
//!
//! Thin shell over `clove-core` and `clove-index`. JSON everywhere; exit codes
//! per DESIGN.md §7.6. The parser lives in [`cli`], output rendering in
//! [`output`], the exit-code table in [`exit`], discovery in [`context`], and
//! each subcommand under [`cmd`].

mod cli;
mod cmd;
mod context;
mod exit;
mod item_json;
mod output;
mod util;

use clap::error::ErrorKind;
use clap::Parser;
use clove_core::{CloveError, ItemStatus, OutputFormat};

use cli::{Cli, Commands};
use context::{discover, Ctx};
use exit::ExitCode;
use output::{emit_error, resolve_format};

fn main() -> std::process::ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
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

    let quiet = cli.quiet;
    let (format, result) = dispatch(cli);
    match result {
        Ok(code) => code.process(),
        Err(error) => emit_error(format, &error, quiet).process(),
    }
}

/// Resolve the output format and run the requested command. Commands that need a
/// repository discover it first (so the format can honor the config default).
fn dispatch(cli: Cli) -> (OutputFormat, Result<ExitCode, CloveError>) {
    let flag = cli.format;
    let no_index = cli.no_index;
    let deep = cli.deep;
    let quiet = cli.quiet;
    let clove_dir = cli.clove_dir.clone();

    match cli.command {
        // Commands that do not require an existing `.clove/`.
        Commands::Version => {
            let f = resolve_format(flag, None);
            (f, cmd::version::run(f).map(|_| ExitCode::Success))
        }
        Commands::Init(args) => {
            let f = resolve_format(flag, None);
            (
                f,
                cmd::init::run(f, clove_dir.as_deref(), args, quiet).map(|_| ExitCode::Success),
            )
        }
        Commands::AgentDoc(args) => {
            let f = resolve_format(flag, None);
            (f, cmd::agent_doc::run(f, args).map(|_| ExitCode::Success))
        }
        // The merge driver is invoked by git on arbitrary file paths; it does
        // not discover a `.clove/` repository.
        Commands::MergeDriver(args) => {
            let f = resolve_format(flag, None);
            // The merge driver returns its own exit code (0 = clean, nonzero =
            // conflict) per the git merge-driver contract.
            (f, cmd::merge_driver::run(f, args))
        }
        // Everything else operates on a discovered repository.
        command => {
            let ctx = match discover(clove_dir.as_deref()) {
                Ok(ctx) => ctx,
                Err(e) => return (resolve_format(flag, None), Err(e)),
            };
            let f = resolve_format(flag, Some(ctx.config.default_format));
            (f, run_repo(command, &ctx, f, no_index, deep, quiet))
        }
    }
}

fn run_repo(
    command: Commands,
    ctx: &Ctx,
    f: OutputFormat,
    no_index: bool,
    deep: bool,
    quiet: bool,
) -> Result<ExitCode, CloveError> {
    let ok = ExitCode::Success;
    match command {
        Commands::New(a) => cmd::new::run(ctx, f, a).map(|_| ok),
        Commands::Show(a) => cmd::show::run(ctx, f, a).map(|_| ok),
        Commands::Edit(a) => cmd::edit::run(ctx, f, a).map(|_| ok),
        Commands::Set(a) => cmd::set::run(ctx, f, a).map(|_| ok),
        Commands::Status(a) => {
            let s = util::parse_status(&a.state)?;
            cmd::status::run(ctx, f, &a.id, s, quiet).map(|_| ok)
        }
        Commands::Start(a) => {
            cmd::status::run(ctx, f, &a.id, ItemStatus::InProgress, quiet).map(|_| ok)
        }
        Commands::Close(a) => {
            cmd::status::run(ctx, f, &a.id, ItemStatus::Closed, quiet).map(|_| ok)
        }
        Commands::Label(a) => cmd::label::run(ctx, f, &a.id, &a.action, &a.label).map(|_| ok),
        Commands::Assign(a) => cmd::assign::run(ctx, f, &a.id, a.assignee, a.clear).map(|_| ok),
        Commands::Priority(a) => cmd::priority::run(ctx, f, &a.id, a.priority).map(|_| ok),
        Commands::Dep(a) => cmd::dep::run(ctx, f, a.action),
        Commands::Ready(a) => cmd::ready::run(ctx, f, a, quiet, no_index, deep).map(|_| ok),
        Commands::Blocked(a) => cmd::blocked::run(ctx, f, a, quiet).map(|_| ok),
        Commands::Ls(a) => cmd::ls::run(ctx, f, a, no_index, deep).map(|_| ok),
        Commands::Query(a) => cmd::query::run(ctx, f, a, no_index, deep).map(|_| ok),
        Commands::Comment(a) => cmd::comments::add(ctx, f, &a.id, &a.message, quiet).map(|_| ok),
        Commands::Comments(a) => cmd::comments::list(ctx, f, &a.id, a.limit).map(|_| ok),
        Commands::Search(a) => cmd::search::run(ctx, f, a, no_index).map(|_| ok),
        Commands::Reindex => cmd::reindex::run(ctx, f, quiet).map(|_| ok),
        Commands::Doctor(a) => cmd::doctor::run(ctx, f, a, no_index),
        Commands::Import(a) => cmd::import::run(ctx, f, a).map(|_| ok),
        Commands::Export(a) => cmd::export::run(ctx, f, a).map(|_| ok),
        // Non-repo commands are dispatched earlier.
        Commands::Version
        | Commands::Init(_)
        | Commands::AgentDoc(_)
        | Commands::MergeDriver(_) => Ok(ok),
    }
}
