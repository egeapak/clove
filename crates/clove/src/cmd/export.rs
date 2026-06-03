//! `clove export <json|jsonl|github> [--out FILE]` (T-M03/T-M04).
//!
//! Phase 1: the `json` and `jsonl` writers. The source of truth is the file
//! store (export always reads files, never the index), loading every item with
//! its full §7.4 shape (frontmatter + body + computed `ready`/`blocked_by`),
//! sorted by `(priority, topological_rank, id)` to match list ordering. Output
//! goes to stdout by default, or atomically to `--out FILE`. The GitHub arm
//! (built with the `github` feature) pushes the shaped items to an `owner/repo`
//! via octocrab; without that feature it returns a clean fallback error.

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
    // Cross-flag validation up front (exit 4) so a misused flag is a clean
    // validation error rather than being silently ignored.
    // `--out` is a file sink; `github` is a network sink and ignores it.
    if matches!(args.export_format, ExportFormat::Github) && args.out.is_some() {
        return Err(CloveError::InvalidField {
            field: "out".to_owned(),
            reason: "`--out` is not valid for `export github` (it pushes to GitHub, not a file)"
                .to_owned(),
        });
    }
    // `target` (owner/repo) is only meaningful for github; reject it on json/jsonl.
    if matches!(args.export_format, ExportFormat::Json | ExportFormat::Jsonl)
        && args.target.is_some()
    {
        return Err(CloveError::InvalidField {
            field: "target".to_owned(),
            reason: "an `owner/repo` target is only valid for `export github`".to_owned(),
        });
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

    // GitHub is a network sink, not a file/stdout sink: it pushes the shaped items
    // (create/update) and emits its own plan/report envelope. `--dry-run` is fully
    // offline (no token needed): it lists what would be pushed from local items.
    if matches!(args.export_format, ExportFormat::Github) {
        let target = args
            .target
            .clone()
            .ok_or_else(|| CloveError::InvalidField {
                field: "target".to_owned(),
                reason: "export github requires an `owner/repo` target".to_owned(),
            })?;
        let objs: Vec<serde_json::Map<String, Value>> = shaped
            .iter()
            .filter_map(|v| v.as_object().cloned())
            .collect();
        return export_github(format, &target, &objs, args.dry_run);
    }

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

/// `clove export github <owner/repo> [--dry-run]`.
///
/// GitHub is a network sink: each shaped local item is created (no
/// `external_ref`) or updated (`external_ref = "gh-<n>"`) via octocrab. The body
/// gets a `<!-- clove-meta: {id,priority,deps} -->` comment appended. `--dry-run`
/// is fully offline — it partitions local items into would-create / would-update
/// without contacting GitHub (no token required). A real push needs `GITHUB_TOKEN`.
#[cfg(feature = "github")]
fn export_github(
    format: OutputFormat,
    target: &str,
    objs: &[serde_json::Map<String, Value>],
    dry_run: bool,
) -> Result<(), CloveError> {
    use crate::output::print_json_success;

    let (plan, report) =
        clove_import::github::export_github(target, objs, dry_run).map_err(export_err)?;

    match (format, report) {
        (OutputFormat::Json | OutputFormat::Jsonl, Some(report)) => print_json_success(
            json!({ "created": report.created, "updated": report.updated }),
            json!({ "warnings": [] }),
        ),
        (OutputFormat::Json | OutputFormat::Jsonl, None) => print_json_success(
            serde_json::to_value(&plan).unwrap_or_else(|_| json!({})),
            json!({ "warnings": [] }),
        ),
        (OutputFormat::Human, Some(report)) => println!(
            "exported to github: {} created, {} updated",
            report.created.len(),
            report.updated.len()
        ),
        (OutputFormat::Human, None) => println!(
            "dry-run: would create {}, would update {}",
            plan.would_create.len(),
            plan.would_update.len()
        ),
    }
    Ok(())
}

/// When built without the `github` feature, `export github` is recognized but
/// fails with a clean error rather than attempting a push.
#[cfg(not(feature = "github"))]
fn export_github(
    _format: OutputFormat,
    _target: &str,
    _objs: &[serde_json::Map<String, Value>],
    _dry_run: bool,
) -> Result<(), CloveError> {
    Err(CloveError::NotYetImplemented {
        feature: "export github (built without github support)".to_owned(),
    })
}

/// Map a `clove-import` error onto a `CloveError` for exit-code classification.
#[cfg(feature = "github")]
fn export_err(err: clove_import::ImportError) -> CloveError {
    use clove_import::ImportError;
    match err {
        ImportError::Core(core) => core,
        ImportError::Source { path, message } => CloveError::Io {
            path,
            source: std::io::Error::other(message),
        },
        other => CloveError::Io {
            path: Utf8Path::new("<github>").to_owned(),
            source: std::io::Error::other(other.to_string()),
        },
    }
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
