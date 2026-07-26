//! Shared filtering, ordering, pagination, and rendering for the list commands
//! (`ls`, `ready`, `blocked`, `query`).

use clove_core::OutputFormat;
use clove_engine::{ListAnswer, Rows};
use serde_json::{json, Map, Value};

use crate::item_json::{frontmatter_object, project};
use crate::output::{print_json_list, print_jsonl_items};

// `Filters` and the ordering contract live in `clove_core::view`, shared by the
// CLI, MCP server, daemon, and web UI.
pub use clove_core::view::Filters;

/// Default cap on list output, so `ls` on a large repo stays snappy (the index
/// steps only this many rows). `_meta.total` still reports the full match count.
pub use clove_core::view::defaults::CLI_LIMIT as DEFAULT_LIST_LIMIT;

/// The CLI's read window, through the shared contract: no `--limit` → the CLI
/// default; `--limit 0` → unlimited; `--limit n` → `n`.
pub fn window(offset: Option<usize>, limit: Option<usize>) -> clove_core::view::Page {
    clove_core::view::Page::new(offset.unwrap_or(0), limit, DEFAULT_LIST_LIMIT)
}

/// Pagination, projection, and metadata options for [`emit`].
#[derive(Debug)]
pub struct ListOpts<'a> {
    /// Match count before pagination.
    pub total: usize,
    /// The requested window, echoed into `_meta`. The engine has already
    /// applied it — see [`emit`].
    pub window: clove_core::view::Page,
    pub fields: Option<&'a [String]>,
    /// Drop null/empty-list keys (and `schema`) from JSON output, through the
    /// same `view::compact_read` the MCP read tools use.
    ///
    /// Note this is the *file* path's shape. The index and daemon paths select a
    /// lean five-column row, so `--compact` there yields fewer keys still —
    /// see `clove_engine::LEAN_FIELDS`.
    pub compact: bool,
    /// Which tier answered: `"daemon"`, `"index"`, or `"files"`. Always
    /// `clove_engine::Source::as_str`, never a literal, so the reported tier
    /// cannot drift from the one that ran.
    pub source: &'a str,
    /// The ordering in force, echoed as `_meta.sort`/`_meta.dir` for the same
    /// reason `_meta.limit` is echoed: so a client can read what was applied
    /// instead of memorizing each surface's default. `search` reports
    /// `"relevance"` here, which is not a `SortField`, so these are plain words.
    pub sort: &'a str,
    pub dir: &'a str,
    /// The filter set in force, echoed as `_meta.filters` for the same reason
    /// `_meta.sort` is echoed: a multi-valued filter set has several spellings
    /// (`--label a --label b`, MCP's `["a","b"]`, the web's `?label=a,b`) and
    /// they all canonicalize, so a client should be able to read back what was
    /// applied rather than assume its input survived.
    ///
    /// `None` for `search`, which takes no field filters — an empty `filters`
    /// object there would claim a surface the command does not have.
    pub filters: Option<&'a Filters>,
    pub warnings: Vec<String>,
}

impl Default for ListOpts<'_> {
    fn default() -> Self {
        ListOpts {
            total: 0,
            window: clove_core::view::Page::default(),
            fields: None,
            compact: false,
            source: "",
            sort: clove_core::view::SortField::Rank.as_str(),
            dir: "asc",
            filters: None,
            warnings: Vec::new(),
        }
    }
}

/// The JSON object for one item in a list. Built either from full frontmatter
/// (file path) or a lean index row; both carry at least id/status/type/priority/
/// title so the human renderer works uniformly.
pub type ListObject = Map<String, Value>;

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

/// The keys [`lean_object`] carries — the only fields the index and daemon fast
/// paths can answer from. Defined by `clove-engine`, which is what decides
/// whether a tier may answer at all.
pub use clove_engine::lean_can_serve;

/// Build the renderable list objects from an engine answer.
///
/// The engine has already chosen the tier and applied the window, so this is a
/// pure shape decision: a lean tier answer becomes the five-column object, and
/// full rows become the whole frontmatter (plus `blocked_by`, which only
/// `blocked` carries). Both go through the *same* [`lean_object`] /
/// [`crate::item_json::frontmatter_object`] builders the file path uses, so the
/// tiers cannot render differently.
pub fn objects_from_answer(answer: &ListAnswer) -> Vec<ListObject> {
    match &answer.rows {
        Rows::Lean(rows) => rows
            .iter()
            .map(|r| lean_object(&r.id, &r.status, &r.item_type, r.priority, &r.title))
            .collect(),
        Rows::Full(rows) => rows
            .iter()
            .map(|r| {
                let mut obj = frontmatter_object(&r.frontmatter);
                if let Some(blocked_by) = &r.blocked_by {
                    obj.insert("blocked_by".to_owned(), json!(blocked_by));
                }
                obj
            })
            .collect(),
    }
}

/// Emit a list: project fields and render in `format`.
///
/// `objects` is **already the requested page** — the engine windows before it
/// returns, because with a filter residue the window and the residue have to be
/// reasoned about together (read-path roadmap §5). `opts.window` is therefore
/// only echoed into `_meta`, never re-applied; applying it twice would skip
/// `offset` rows a second time.
pub fn emit(format: OutputFormat, objects: Vec<ListObject>, opts: ListOpts<'_>) {
    let page: Vec<&ListObject> = objects.iter().collect();

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let values: Vec<Value> = page
                .iter()
                .map(|obj| {
                    let obj = match opts.fields {
                        Some(f) => project((*obj).clone(), f),
                        None => (*obj).clone(),
                    };
                    let obj = if opts.compact {
                        clove_core::view::compact_read(obj)
                    } else {
                        obj
                    };
                    Value::Object(obj)
                })
                .collect();
            if matches!(format, OutputFormat::Jsonl) {
                print_jsonl_items(&values);
            } else {
                let mut meta = json!({
                    "limit": opts.window.reported_limit(),
                    "total": opts.total,
                    "returned": page.len(),
                    "offset": opts.window.offset,
                    "sort": opts.sort,
                    "dir": opts.dir,
                    "source": opts.source,
                    "warnings": opts.warnings,
                });
                if let (Some(filters), Some(map)) = (opts.filters, meta.as_object_mut()) {
                    map.insert(
                        "filters".to_owned(),
                        serde_json::to_value(filters).unwrap_or(Value::Null),
                    );
                }
                print_json_list(values, meta);
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
