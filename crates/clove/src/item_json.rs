//! Item → JSON shaping for command output (DESIGN.md §7.4).
//!
//! The on-disk frontmatter serializes via its derived `Serialize`; commands
//! augment that object with computed fields (`body`, `comment_count`, `ready`,
//! `blocked_by`) and may project it down to a `--fields` subset.

use std::collections::HashMap;

use camino::Utf8Path;
use clove_core::{list_comments, CloveId, Item, ItemFrontmatter, OutputFormat};
use serde_json::{json, Map, Value};

use crate::output::print_json_success;

/// The base JSON object for an item: exactly its serialized frontmatter
/// (`id`, `title`, `status`, `type`, `priority`, timestamps, `labels`, `deps`, …).
pub fn item_object(item: &Item) -> Map<String, Value> {
    frontmatter_object(&item.frontmatter)
}

/// The JSON object for an item's frontmatter alone (the list fast path, which
/// never reads bodies).
pub fn frontmatter_object(fm: &ItemFrontmatter) -> Map<String, Value> {
    match serde_json::to_value(fm) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Build the full §7.4 item JSON object for export: the serialized frontmatter
/// plus the computed/augmented fields `body`, `comment_count`, `ready`,
/// `blocked_by`, and `dangling_deps`. This is the same shape `clove show
/// --format json` emits, so an exported item validates against `item.json`.
///
/// `graph`, `ready`, and `blocked` are precomputed once over the whole store by
/// the caller and shared across all items (avoiding a per-item rescan).
pub fn export_object(
    item: &Item,
    issues_dir: &Utf8Path,
    ready: &std::collections::HashSet<CloveId>,
    blocked: &HashMap<CloveId, (Vec<CloveId>, Vec<CloveId>)>,
) -> Map<String, Value> {
    let id = &item.frontmatter.id;
    let mut obj = item_object(item);
    obj.insert("body".to_owned(), json!(item.body));

    let comment_count = list_comments(issues_dir, id).map(|c| c.len()).unwrap_or(0);
    obj.insert("comment_count".to_owned(), json!(comment_count));

    obj.insert("ready".to_owned(), json!(ready.contains(id)));

    let (blocked_by, dangling) = match blocked.get(id) {
        Some((blocking, dangling)) => {
            let combined: Vec<String> = blocking
                .iter()
                .chain(dangling.iter())
                .map(CloveId::to_string)
                .collect();
            (combined, dangling.iter().map(CloveId::to_string).collect())
        }
        None => (Vec::new(), Vec::new()),
    };
    obj.insert("blocked_by".to_owned(), json!(blocked_by));
    obj.insert("dangling_deps".to_owned(), json!(dangling));

    obj
}

/// Restrict `obj` to the keys named in `fields` (order follows `fields`).
/// Unknown field names are ignored.
pub fn project(obj: Map<String, Value>, fields: &[String]) -> Map<String, Value> {
    let mut out = Map::new();
    for field in fields {
        if let Some(value) = obj.get(field) {
            out.insert(field.clone(), value.clone());
        }
    }
    out
}

/// Print a single item after a mutation: the full JSON object, or a one-line
/// human summary. `extra` fields (e.g. computed `ready`) are merged in.
pub fn print_item(format: OutputFormat, item: &Item, extra: Map<String, Value>) {
    let mut obj = item_object(item);
    obj.extend(extra);
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_success(Value::Object(obj), json!({ "warnings": [] }))
        }
        OutputFormat::Human => print_human(item),
    }
}

/// A compact one-line human rendering of an item.
pub fn print_human(item: &Item) {
    let fm = &item.frontmatter;
    println!(
        "{}  [{}] p{} {}  {}",
        fm.id.as_str(),
        fm.status.as_str(),
        fm.priority.get(),
        fm.item_type.as_str(),
        fm.title
    );
}

/// Parse a comma-separated `--fields` list into a vector of field names.
pub fn parse_fields(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
