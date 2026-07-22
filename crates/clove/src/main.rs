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
mod mux_help;
mod output;
mod plugin;
mod util;

use clap::error::ErrorKind;
use clap::Parser;
use clove_core::OutputFormat;
use clove_types::{CloveError, ItemStatus};

use cli::{Cli, Commands};
use context::{discover, Ctx};
use exit::ExitCode;
use output::{emit_error, resolve_format};

fn main() -> std::process::ExitCode {
    // Intercept a bare `<mux> --help` (`import`/`export`/`sync`) before clap parses
    // (PLUGIN_REGISTRY.md §6): its help trailer lists the installed provider
    // plugins, which clap's compile-time `after_help` cannot. Every other argv is
    // untouched and falls through to the normal parser below.
    let argv: Vec<String> = std::env::args().collect();
    if let Some(mux) = mux_help::detect(&argv) {
        mux_help::render(mux);
        return std::process::ExitCode::SUCCESS;
    }

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
    let color = cli.color;
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
        // `setup` configures Claude Code (settings.json + CLOVE.md) and must run
        // before `clove init`, so it does not require a `.clove/` repository.
        Commands::Setup(args) => {
            let f = resolve_format(flag, None);
            (
                f,
                cmd::setup::run(
                    f,
                    args.global,
                    args.dry_run,
                    args.claude_dir.as_deref(),
                    quiet,
                )
                .map(|_| ExitCode::Success),
            )
        }
        // The merge driver is invoked by git on arbitrary file paths; it does
        // not discover a `.clove/` repository.
        Commands::MergeDriver(args) => {
            let f = resolve_format(flag, None);
            // The merge driver returns its own exit code (0 = clean, nonzero =
            // conflict) per the git merge-driver contract.
            (f, cmd::merge_driver::run(f, args))
        }
        // The MCP server owns stdin/stdout for JSON-RPC framing (so it ignores
        // `--format`) and must start even without an initialized repo: a plugin
        // spawns it per session, and its tools surface a "no clove repository"
        // error until `clove init` runs, instead of the server failing to launch.
        Commands::Mcp => {
            let f = resolve_format(flag, None);
            (
                f,
                cmd::mcp::run(clove_dir.as_deref()).map(|_| ExitCode::Success),
            )
        }
        // `plugin list` is a pure `stat` walk of the search path; it needs no
        // repository (so `clove plugin list` works before `clove init`).
        Commands::Plugin(_) => {
            let f = resolve_format(flag, None);
            (f, cmd::plugin::run(f).map(|_| ExitCode::Success))
        }
        // An external plugin (`clove-<name>`). Resolution runs *before* discovery:
        // an unknown subcommand is a usage error (exit 1) that needs no repo, so
        // `clove frobnicate` outside a repo still exits 1 rather than NoRepo. Only
        // a plugin that actually resolves requires the repo context it will run in.
        Commands::External(argv) => dispatch_external(
            flag,
            no_index,
            deep,
            quiet,
            color,
            clove_dir.as_deref(),
            argv,
        ),
        // Everything else operates on a discovered repository.
        command => {
            let ctx = match discover(clove_dir.as_deref()) {
                Ok(ctx) => ctx,
                Err(e) => return (resolve_format(flag, None), Err(e)),
            };
            let f = resolve_format(flag, Some(ctx.config.default_format));
            (f, run_repo(command, &ctx, f, no_index, deep, quiet, color))
        }
    }
}

/// Route a multiplexer subcommand (`import`/`export`/`sync`) whose provider is
/// not a built-in to a `clove-<multiplexer>-<provider>` plugin (PLUGIN_SYSTEM.md
/// §4.2). `sync` has no built-in providers, so every `clove sync` reaches here.
///
/// A resolved plugin is exec'd with `rest` forwarded and the provider threaded
/// into `$CLOVE_PROVIDER` (§6.2); a miss is a validation error (exit 4) scoped to
/// the multiplexer — never a fall-back to a generic `clove-<provider>` (§4.3).
fn dispatch_multiplexer(
    multiplexer: &str,
    provider: &str,
    rest: &[String],
    ctx: &Ctx,
    globals: &plugin::PluginGlobals,
) -> Result<ExitCode, CloveError> {
    match plugin::resolve(&[multiplexer, provider]) {
        Some(path) => plugin::run_plugin(
            &path,
            rest,
            &[multiplexer, provider],
            ctx,
            globals,
            multiplexer,
            Some(provider),
        ),
        None => Err(CloveError::InvalidField {
            field: "provider".to_owned(),
            reason: format!(
                "unknown {multiplexer} provider `{provider}`; install clove-{multiplexer}-{provider}"
            ),
        }),
    }
}

/// Dispatch a `Commands::External(argv)` plugin invocation (PLUGIN_SYSTEM.md
/// §4.1). `argv[0]` is the subcommand name.
///
/// Resolution comes first: a `clove-<name>` that resolves nowhere is a usage
/// error (exit 1) — named binary + installed-plugin list — and needs no
/// repository. Only a plugin that *does* resolve requires the repo context (it
/// reads/writes the store via `CLOVE_DIR` …), so discovery runs on the hit path
/// and a missing `.clove/` surfaces as the standard `NoRepo` error there.
fn dispatch_external(
    flag: Option<OutputFormat>,
    no_index: bool,
    deep: bool,
    quiet: bool,
    color: cli::ColorChoice,
    clove_dir: Option<&camino::Utf8Path>,
    argv: Vec<String>,
) -> (OutputFormat, Result<ExitCode, CloveError>) {
    let Some(name) = argv.first().cloned() else {
        // `external_subcommand` always yields at least the subcommand token, so
        // this is unreachable in practice; treat an empty argv as a usage error.
        let f = resolve_format(flag, None);
        return (
            f,
            Ok(output::emit_unknown_subcommand(f, "", "clove-", &[], quiet)),
        );
    };

    let Some(path) = plugin::resolve(&[name.as_str()]) else {
        let f = resolve_format(flag, None);
        let installed: Vec<String> = plugin::list().into_iter().map(|p| p.name).collect();
        let binary = format!("clove-{name}{}", std::env::consts::EXE_SUFFIX);
        return (
            f,
            Ok(output::emit_unknown_subcommand(
                f, &name, &binary, &installed, quiet,
            )),
        );
    };

    let ctx = match discover(clove_dir) {
        Ok(ctx) => ctx,
        Err(e) => return (resolve_format(flag, None), Err(e)),
    };
    let f = resolve_format(flag, Some(ctx.config.default_format));
    let globals = plugin::PluginGlobals {
        format: f,
        color,
        quiet,
        no_index,
        deep,
    };
    let rest = argv[1..].to_vec();
    let result = plugin::run_plugin(&path, &rest, &[name.as_str()], &ctx, &globals, &name, None);
    (f, result)
}

fn run_repo(
    command: Commands,
    ctx: &Ctx,
    f: OutputFormat,
    no_index: bool,
    deep: bool,
    quiet: bool,
    color: cli::ColorChoice,
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
        Commands::Dep(a) => cmd::dep::run(ctx, f, a.action, no_index),
        Commands::Ready(a) => cmd::ready::run(ctx, f, a, quiet, no_index, deep).map(|_| ok),
        Commands::Blocked(a) => cmd::blocked::run(ctx, f, a, quiet, no_index).map(|_| ok),
        Commands::Ls(a) => cmd::ls::run(ctx, f, a, no_index, deep).map(|_| ok),
        Commands::Query(a) => cmd::query::run(ctx, f, a, no_index, deep).map(|_| ok),
        Commands::Comment(a) => cmd::comments::add(ctx, f, &a.id, &a.message, quiet).map(|_| ok),
        Commands::Comments(a) => cmd::comments::list(ctx, f, &a.id, a.limit).map(|_| ok),
        Commands::Search(a) => cmd::search::run(ctx, f, a, no_index).map(|_| ok),
        Commands::Stats(a) => cmd::stats::run(ctx, f, a, no_index).map(|_| ok),
        Commands::Reindex => cmd::reindex::run(ctx, f, quiet).map(|_| ok),
        Commands::Doctor(a) => cmd::doctor::run(ctx, f, a, no_index),
        Commands::Daemon(a) => cmd::daemon::run(ctx, f, a.action),
        Commands::Tui => cmd::tui::run(ctx, f).map(|_| ok),
        Commands::Serve(a) => cmd::serve::run(ctx, a, quiet).map(|_| ok),
        // `import` mirrors `export`: the built-in native formats (`json`/`jsonl`,
        // clove's own restore) parse their own `rest`; any other provider
        // (`tk`, `beads`, …) falls through to a `clove-import-<provider>` plugin
        // (PLUGIN_SYSTEM.md §4.2).
        Commands::Import(a) => {
            if cmd::import::is_builtin(&a.provider) {
                cmd::import::run(ctx, f, a)
            } else {
                let globals = plugin::PluginGlobals {
                    format: f,
                    color,
                    quiet,
                    no_index,
                    deep,
                };
                dispatch_multiplexer("import", &a.provider, &a.rest, ctx, &globals)
            }
        }
        // `export` is a pure router: the built-in file formats (`json`/`jsonl`)
        // parse their own `rest`; any other provider falls through to a
        // `clove-export-<provider>` plugin (PLUGIN_SYSTEM.md §4.2).
        Commands::Export(a) => {
            if cmd::export::is_builtin(&a.provider) {
                cmd::export::run(ctx, f, a)
            } else {
                let globals = plugin::PluginGlobals {
                    format: f,
                    color,
                    quiet,
                    no_index,
                    deep,
                };
                dispatch_multiplexer("export", &a.provider, &a.rest, ctx, &globals)
            }
        }
        // `sync` is a pure router with no built-in providers: every provider
        // (including `github`) falls through to a `clove-sync-<provider>` plugin
        // (PLUGIN_SYSTEM.md §4.2).
        Commands::Sync(a) => {
            let globals = plugin::PluginGlobals {
                format: f,
                color,
                quiet,
                no_index,
                deep,
            };
            dispatch_multiplexer("sync", &a.provider, &a.rest, ctx, &globals)
        }
        // Non-repo commands and external plugins are dispatched earlier.
        Commands::Version
        | Commands::Init(_)
        | Commands::AgentDoc(_)
        | Commands::Setup(_)
        | Commands::MergeDriver(_)
        | Commands::Plugin(_)
        | Commands::External(_)
        | Commands::Mcp => Ok(ok),
    }
}
