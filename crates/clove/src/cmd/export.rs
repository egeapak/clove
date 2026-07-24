//! `clove export <json|jsonl> [--out FILE]` (T-M04).
//!
//! The `json` and `jsonl` writers. The source of truth is the file store (export
//! always reads files, never the index), loading every item with its full §7.4
//! shape (frontmatter + body + computed `ready`/`blocked_by`), sorted by
//! `(priority, topological_rank, id)` to match list ordering. Output goes to
//! stdout by default, or atomically to `--out FILE`. (GitHub is no longer an
//! export sink — `clove sync github` is the single GitHub path.)

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use camino::{Utf8Path, Utf8PathBuf};
use clap::error::ErrorKind;
use clap::Parser;
use clove_core::{GraphStore, OutputFormat};
use clove_import::export::{export_json, export_jsonl};
use clove_types::{CloveError, CloveId};
use serde_json::{json, Value};
use tempfile::NamedTempFile;

use crate::cli::{ExportArgs, ExportFormat};
use crate::cmd::listing::sort_by_priority_topo;
use crate::context::Ctx;
use crate::exit::ExitCode;
use crate::item_json::export_object;

/// The built-in export providers (pure file formats). Any other provider falls
/// through to a `clove-export-<provider>` plugin (handled in `main::run_repo`).
pub fn is_builtin(provider: &str) -> bool {
    matches!(provider, "json" | "jsonl")
}

// Inner-parsed from `rest` (the format itself comes from `provider`, not a
// positional) so the top-level router can forward `rest` raw for the plugin
// fall-through. The `about` is user-facing (shown on `--help`); this comment is
// not.
#[derive(Debug, Parser)]
#[command(
    name = "clove export <json|jsonl>",
    no_binary_name = true,
    about = "Arguments for a built-in export provider (json/jsonl)"
)]
struct ExportBuiltinArgs {
    /// Write to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<Utf8PathBuf>,
}

/// Shape every item in the store into the canonical §7.4 export object
/// (frontmatter + body + computed `ready`/`blocked_by`), in the canonical
/// `(priority, topological rank, id)` order.
fn shaped_objects(ctx: &Ctx) -> Result<Vec<serde_json::Map<String, Value>>, CloveError> {
    // Files are the source of truth: load every item with its body.
    let (items, _errors) = ctx.store.scan()?;

    let frontmatters: Vec<_> = items.iter().map(|i| i.frontmatter.clone()).collect();
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ranks = graph.topological_ranks();

    let ready: HashSet<CloveId> = graph.ready_items().into_iter().collect();
    let blocked: HashMap<CloveId, (Vec<CloveId>, Vec<CloveId>)> = graph
        .blocked_items()
        .into_iter()
        .map(|b| (b.id, (b.blocking_deps, b.dangling_deps)))
        .collect();

    let mut order = frontmatters.clone();
    sort_by_priority_topo(&mut order, &ranks);
    let by_id: HashMap<&CloveId, &clove_types::Item> =
        items.iter().map(|i| (&i.frontmatter.id, i)).collect();

    Ok(order
        .iter()
        .filter_map(|fm| by_id.get(&fm.id).copied())
        .map(|item| export_object(item, &ctx.issues_dir, &ready, &blocked))
        .collect())
}

/// Run a built-in export provider (`json`/`jsonl`). `args.provider` is guaranteed
/// a built-in by [`is_builtin`]; `args.rest` is inner-parsed into the optional
/// `--out`. A parse failure prints clap's error and exits usage-class (mirroring
/// `main`).
pub fn run(ctx: &Ctx, format: OutputFormat, args: ExportArgs) -> Result<ExitCode, CloveError> {
    let export_format = match args.provider.as_str() {
        "json" => ExportFormat::Json,
        "jsonl" => ExportFormat::Jsonl,
        // `is_builtin` gates this call; any other provider is dispatched to a
        // plugin before we get here.
        other => unreachable!("non-built-in export provider `{other}` reached built-in run"),
    };

    let parsed = match ExportBuiltinArgs::try_parse_from(args.rest.iter().cloned()) {
        Ok(parsed) => parsed,
        Err(err) => {
            let _ = err.print();
            // A global flag placed *after* the provider lands in `rest` and reads
            // as an unknown argument here; clap's default `-- --format` tip is
            // wrong for that case, so point at the real fix.
            if err.kind() == ErrorKind::UnknownArgument {
                eprintln!(
                    "\nnote: clove global flags (--format, --color, --quiet, …) must come \
                     before the provider, e.g. `clove export --format json {} …`",
                    args.provider
                );
            }
            return Ok(match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => ExitCode::Success,
                _ => ExitCode::Usage,
            });
        }
    };

    // Files are the source of truth: shape every item (body + computed fields) in
    // the canonical order. Per-file parse failures are dropped (consistent with
    // `ls`/`ready`).
    let shaped: Vec<Value> = shaped_objects(ctx)?
        .into_iter()
        .map(Value::Object)
        .collect();

    // Pick the sink: a file (atomic write) when `--out` is set, else stdout.
    match &parsed.out {
        Some(path) => {
            let mut buf = Vec::new();
            serialize(export_format, &shaped, &mut buf).map_err(|source| CloveError::Io {
                path: path.clone(),
                source,
            })?;
            atomic_write(path, &buf)?;
            // Under non-JSON, a short confirmation goes to *stderr* only; stdout
            // stays empty so a redirect of the file path is uncluttered.
            if matches!(format, OutputFormat::Human) {
                eprintln!("wrote {} items to {path}", shaped.len());
            }
        }
        None => {
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            serialize(export_format, &shaped, &mut handle).map_err(|source| CloveError::Io {
                path: Utf8Path::new("<stdout>").to_owned(),
                source,
            })?;
        }
    }

    Ok(ExitCode::Success)
}

/// Serialize `items` to `writer` in the chosen format. JSON wraps them in the
/// standard envelope; JSONL emits one bare item object per line.
fn serialize<W: Write>(
    fmt: ExportFormat,
    items: &[Value],
    writer: &mut W,
) -> Result<(), io::Error> {
    match fmt {
        ExportFormat::Json => export_json(writer, items, json!({ "source": "files" })),
        ExportFormat::Jsonl => export_jsonl(writer, items),
    }
}

/// Atomically write `bytes` to `path`: a tempfile in the same directory followed
/// by a rename, so a reader never sees a partial export.
fn atomic_write(path: &Utf8Path, bytes: &[u8]) -> Result<(), CloveError> {
    let parent = path.parent().unwrap_or_else(|| Utf8Path::new("."));
    let io_err = |source| CloveError::Io {
        path: path.to_owned(),
        source,
    };
    let mut temp = NamedTempFile::new_in(parent.as_std_path()).map_err(io_err)?;
    temp.write_all(bytes).map_err(io_err)?;
    temp.flush().map_err(io_err)?;
    temp.persist(path.as_std_path())
        .map_err(|e| io_err(e.error))?;
    Ok(())
}
