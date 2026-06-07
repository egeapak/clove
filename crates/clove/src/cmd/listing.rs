//! Shared filtering, ordering, pagination, and rendering for the list commands
//! (`ls`, `ready`, `blocked`, `query`).

use std::collections::HashMap;

use clove_core::{GraphStore, OutputFormat};
use clove_index::ItemListRow;
use clove_types::{CloveId, ItemFrontmatter};
use serde_json::{json, Map, Value};

use crate::item_json::{frontmatter_object, project};
use crate::output::{print_json_list, print_jsonl_items};

// `Filters` and the canonical `(priority, topo, id)` ordering live in
// `clove_core::view`, shared by the CLI, MCP server, and web UI.
pub use clove_core::view::sort_by_rank as sort_by_priority_topo;
pub use clove_core::view::Filters;

/// Default cap on list output, so `ls` on a large repo stays snappy (the index
/// steps only this many rows). `_meta.total` still reports the full match count.
pub const DEFAULT_LIST_LIMIT: usize = 100;

/// Resolve the effective page limit: `None` flag → the default cap; `--limit 0`
/// → unlimited; `--limit n` → `n`.
pub fn effective_limit(arg: Option<usize>) -> Option<usize> {
    match arg {
        None => Some(DEFAULT_LIST_LIMIT),
        Some(0) => None,
        Some(n) => Some(n),
    }
}

/// Pagination, projection, and metadata options for [`emit`].
#[derive(Debug, Default)]
pub struct ListOpts<'a> {
    /// Match count before pagination.
    pub total: usize,
    pub offset: usize,
    pub limit: Option<usize>,
    pub fields: Option<&'a [String]>,
    /// `"files"` or `"index"`.
    pub source: &'a str,
    pub warnings: Vec<String>,
}

/// The JSON object for one item in a list. Built either from full frontmatter
/// (file path) or a lean index row; both carry at least id/status/type/priority/
/// title so the human renderer works uniformly.
pub type ListObject = Map<String, Value>;

/// Build list objects from full frontmatter (the file-scan path).
pub fn objects_from_frontmatters(fms: &[ItemFrontmatter]) -> Vec<ListObject> {
    fms.iter().map(frontmatter_object).collect()
}

/// The single lean-object builder, shared by the index path and the daemon path
/// so their output is byte-identical. The lean shape is
/// `{ id, status, type, priority, title }` — the columns `ls` renders.
fn lean_object(id: &str, status: &str, item_type: &str, priority: u8, title: &str) -> ListObject {
    let mut m = Map::new();
    m.insert("id".to_owned(), Value::String(id.to_owned()));
    m.insert("status".to_owned(), Value::String(status.to_owned()));
    m.insert("type".to_owned(), Value::String(item_type.to_owned()));
    m.insert("priority".to_owned(), Value::Number(priority.into()));
    m.insert("title".to_owned(), Value::String(title.to_owned()));
    m
}

/// Build list objects from lean index rows (the index fast path).
pub fn objects_from_lean_rows(rows: &[ItemListRow]) -> Vec<ListObject> {
    rows.iter()
        .map(|r| {
            lean_object(
                r.id.as_str(),
                r.status.as_str(),
                r.item_type.as_str(),
                r.priority,
                &r.title,
            )
        })
        .collect()
}

/// Build list objects from daemon-returned wire rows (the daemon fast path).
/// Uses the same [`lean_object`] builder as the index path, guaranteeing parity.
pub fn objects_from_wire_rows(rows: &[clove_ipc::LeanRow]) -> Vec<ListObject> {
    rows.iter()
        .map(|r| lean_object(&r.id, &r.status, &r.item_type, r.priority, &r.title))
        .collect()
}

/// Emit a list: apply offset/limit, project fields, and render in `format`.
/// Objects are pre-built so the index path can pass a lean projection and the
/// file path the full frontmatter, through one renderer.
pub fn emit(format: OutputFormat, objects: Vec<ListObject>, opts: ListOpts<'_>) {
    let page: Vec<&ListObject> = objects
        .iter()
        .skip(opts.offset)
        .take(opts.limit.unwrap_or(usize::MAX))
        .collect();

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let values: Vec<Value> = page
                .iter()
                .map(|obj| {
                    let obj = match opts.fields {
                        Some(f) => project((*obj).clone(), f),
                        None => (*obj).clone(),
                    };
                    Value::Object(obj)
                })
                .collect();
            if matches!(format, OutputFormat::Jsonl) {
                print_jsonl_items(&values);
            } else {
                print_json_list(
                    values,
                    json!({
                        "total": opts.total,
                        "returned": page.len(),
                        "offset": opts.offset,
                        "source": opts.source,
                        "warnings": opts.warnings,
                    }),
                );
            }
        }
        OutputFormat::Human => {
            for obj in &page {
                let s = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or("");
                let priority = obj.get("priority").and_then(Value::as_u64).unwrap_or(0);
                println!(
                    "{}  [{}] p{} {}  {}",
                    s("id"),
                    s("status"),
                    priority,
                    s("type"),
                    s("title")
                );
            }
        }
    }
}

/// Build the dependency graph and its topological ranks from a frontmatter set.
pub fn ranks_of(frontmatters: &[ItemFrontmatter]) -> (GraphStore, HashMap<CloveId, usize>) {
    let (graph, _dangling) = GraphStore::build(frontmatters);
    let ranks = graph.topological_ranks();
    (graph, ranks)
}
