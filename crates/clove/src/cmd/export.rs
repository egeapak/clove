//! `clove export <json|jsonl> [--out FILE]` (T-M04).
//!
//! The `json` and `jsonl` writers. The source of truth is the file store (export
//! always reads files, never the index), loading every item with its full §7.4
//! shape (frontmatter + body + computed `ready`/`blocked_by`), sorted by
//! `(priority, topological_rank, id)` to match list ordering. Items are read and
//! written one at a time, so memory does not grow with the bodies. An item file
//! that does not parse is left out and named in a warning (stderr, and
//! `_meta.warnings` for `json`). Output goes to stdout by default, or atomically
//! to `--out FILE`. (GitHub is no longer an
//! export sink — `clove sync github` is the single GitHub path.)

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use camino::{Utf8Path, Utf8PathBuf};
use clap::error::ErrorKind;
use clap::Parser;
use clove_core::{GraphStore, OutputFormat, ScanError};
use clove_import::export::export_json_streamed;
use clove_types::{CloveError, CloveId};
use serde_json::{json, Value};
use tempfile::NamedTempFile;

use crate::cli::{ExportArgs, ExportFormat};
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

/// The export plan: every item id in the canonical `(priority, topological
/// rank, id)` order, the graph-derived `ready`/`blocked` sets, and one warning
/// per item file that did not parse.
///
/// Built from the body-free frontmatter scan, so planning holds no bodies; the
/// bodies are read one item at a time while writing ([`write_items`]).
struct Plan {
    order: Vec<CloveId>,
    ready: HashSet<CloveId>,
    blocked: HashMap<CloveId, (Vec<CloveId>, Vec<CloveId>)>,
    warnings: Vec<String>,
}

fn plan(ctx: &Ctx) -> Result<Plan, CloveError> {
    // Files are the source of truth.
    let (mut frontmatters, errors) = ctx.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ranks = graph.topological_ranks();

    let ready: HashSet<CloveId> = graph.ready_items().into_iter().collect();
    let blocked: HashMap<CloveId, (Vec<CloveId>, Vec<CloveId>)> = graph
        .blocked_items()
        .into_iter()
        .map(|b| (b.id, (b.blocking_deps, b.dangling_deps)))
        .collect();

    // Export is not a user-facing list: its order is fixed to the canonical
    // `(priority, topo, id)` so an export → import round-trip is byte-stable.
    clove_core::view::Order::default().apply(&mut frontmatters, &ranks);

    Ok(Plan {
        order: frontmatters.into_iter().map(|fm| fm.id).collect(),
        ready,
        blocked,
        // An export is a backup: an item file left out of it must be named,
        // or a later restore loses the item without anyone having been told.
        warnings: errors.iter().map(skipped_warning).collect(),
    })
}

fn skipped_warning(error: &ScanError) -> String {
    format!("skipped (not exported): {error}")
}

/// The planned items as export objects, each read from disk only when pulled.
/// An item whose file stops parsing between the plan and the read (or whose
/// body alone is invalid) becomes a warning instead of an item.
fn objects<'a>(
    ctx: &'a Ctx,
    plan: &'a Plan,
    late_warnings: &'a RefCell<Vec<String>>,
) -> impl Iterator<Item = Value> + 'a {
    plan.order.iter().filter_map(move |id| match ctx.store.get(id) {
        Ok(item) => Some(Value::Object(export_object(
            &item,
            &ctx.issues_dir,
            &plan.ready,
            &plan.blocked,
        ))),
        Err(error) => {
            late_warnings
                .borrow_mut()
                .push(format!("skipped (not exported): {id}: {error}"));
            None
        }
    })
}

/// Write the export to `writer`, one item at a time, and return the item count
/// and every warning.
fn write_items<W: Write>(
    ctx: &Ctx,
    fmt: ExportFormat,
    plan: &Plan,
    writer: &mut W,
) -> Result<(usize, Vec<String>), io::Error> {
    let late = RefCell::new(Vec::new());
    let all_warnings = || -> Vec<String> {
        plan.warnings
            .iter()
            .cloned()
            .chain(late.borrow().iter().cloned())
            .collect()
    };
    let mut items = objects(ctx, plan, &late);
    let count = match fmt {
        ExportFormat::Json => export_json_streamed(writer, &mut items, &mut || {
            json!({ "source": "files", "warnings": all_warnings() })
        })?,
        ExportFormat::Jsonl => {
            let mut count = 0;
            for item in items {
                serde_json::to_writer(&mut *writer, &item)?;
                writer.write_all(b"\n")?;
                count += 1;
            }
            count
        }
    };
    writer.flush()?;
    Ok((count, all_warnings()))
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

    let plan = plan(ctx)?;

    // Pick the sink: a file (atomic: temp file + rename) when `--out` is set,
    // else stdout. Either way the items stream through, so memory does not grow
    // with the total size of the bodies.
    let (count, warnings) = match &parsed.out {
        Some(path) => {
            let parent = match path.parent() {
                Some(parent) if !parent.as_str().is_empty() => parent,
                _ => Utf8Path::new("."),
            };
            let io_err = |source| CloveError::Io {
                path: path.clone(),
                source,
            };
            let temp = NamedTempFile::new_in(parent.as_std_path()).map_err(io_err)?;
            let mut writer = io::BufWriter::new(temp);
            let written = write_items(ctx, export_format, &plan, &mut writer).map_err(io_err)?;
            let temp = writer.into_inner().map_err(|e| io_err(e.into_error()))?;
            temp.persist(path.as_std_path())
                .map_err(|e| io_err(e.error))?;
            // Under non-JSON, a short confirmation goes to *stderr* only; stdout
            // stays empty so a redirect of the file path is uncluttered.
            if matches!(format, OutputFormat::Human) {
                eprintln!("wrote {} items to {path}", written.0);
            }
            written
        }
        None => {
            let stdout = io::stdout();
            let mut writer = io::BufWriter::new(stdout.lock());
            write_items(ctx, export_format, &plan, &mut writer).map_err(|source| {
                clove_plugin::stdout::exit_if_broken_pipe(&source);
                CloveError::Io {
                    path: Utf8Path::new("<stdout>").to_owned(),
                    source,
                }
            })?
        }
    };

    // On stderr in every format: `jsonl` has no envelope to carry warnings, and
    // a `json` export redirected to a file would otherwise hide them.
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }

    Ok(ExitCode::Success)
}
