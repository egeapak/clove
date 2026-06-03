//! `clove export <json|jsonl|github> [--out FILE]` (T-M03/T-M04).
//!
//! Phase 1: the `json` and `jsonl` writers. The source of truth is the file
//! store (export always reads files, never the index), loading every item with
//! its full §7.4 shape (frontmatter + body + computed `ready`/`blocked_by`),
//! sorted by `(priority, topological_rank, id)` to match list ordering. Output
//! goes to stdout by default, or atomically to `--out FILE`. The GitHub arm is
//! Phase 5 and still returns [`CloveError::NotYetImplemented`].

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use camino::Utf8Path;
use clove_core::{CloveError, CloveId, GraphStore, OutputFormat};
use clove_import::export::{export_json, export_jsonl};
use serde_json::{json, Value};
use tempfile::NamedTempFile;

use crate::cli::{ExportArgs, ExportFormat};
use crate::cmd::listing::sort_by_priority_topo;
use crate::context::Ctx;
use crate::item_json::export_object;

pub fn run(ctx: &Ctx, format: OutputFormat, args: ExportArgs) -> Result<(), CloveError> {
    match args.export_format {
        ExportFormat::Json | ExportFormat::Jsonl => {}
        ExportFormat::Github => {
            return Err(CloveError::NotYetImplemented {
                feature: "export github".to_owned(),
            });
        }
    }

    // Files are the source of truth: load every item with its body. Per-file
    // parse failures are dropped (consistent with `ls`/`ready`); export never
    // partially succeeds on a corrupt store silently beyond what scan reports.
    let (items, _errors) = ctx.store.scan()?;

    // Build the graph once over the whole store for ranks + ready/blocked_by.
    let frontmatters: Vec<_> = items.iter().map(|i| i.frontmatter.clone()).collect();
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ranks = graph.topological_ranks();

    let ready: HashSet<CloveId> = graph.ready_items().into_iter().collect();
    let blocked: HashMap<CloveId, (Vec<CloveId>, Vec<CloveId>)> = graph
        .blocked_items()
        .into_iter()
        .map(|b| (b.id, (b.blocking_deps, b.dangling_deps)))
        .collect();

    // Sort frontmatter to derive the canonical `(priority, topo rank, id)` order,
    // then shape the full items in that order.
    let mut order = frontmatters.clone();
    sort_by_priority_topo(&mut order, &ranks);
    let by_id: HashMap<&CloveId, &clove_core::Item> =
        items.iter().map(|i| (&i.frontmatter.id, i)).collect();

    let shaped: Vec<Value> = order
        .iter()
        .filter_map(|fm| by_id.get(&fm.id).copied())
        .map(|item| Value::Object(export_object(item, &ctx.issues_dir, &ready, &blocked)))
        .collect();

    // Pick the sink: a file (atomic write) when `--out` is set, else stdout.
    match &args.out {
        Some(path) => {
            let mut buf = Vec::new();
            serialize(args.export_format, &shaped, &mut buf).map_err(|source| CloveError::Io {
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
            serialize(args.export_format, &shaped, &mut handle).map_err(|source| {
                CloveError::Io {
                    path: Utf8Path::new("<stdout>").to_owned(),
                    source,
                }
            })?;
        }
    }

    Ok(())
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
        ExportFormat::Github => unreachable!("github handled before serialize"),
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
