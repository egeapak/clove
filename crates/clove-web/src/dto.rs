//! Item → JSON shaping for the web API.
//!
//! Reuses the on-disk frontmatter's derived `Serialize` (so the shape matches
//! DESIGN §7.4 and the CLI's `clove show --format json`) and augments it with the
//! graph-computed fields `ready`, `blocked_by`, `dangling_deps` — plus `body` and
//! `comment_count` for the detail view. The graph is built once per request and
//! shared across all items via [`GraphContext`].

use std::collections::{HashMap, HashSet};

use camino::Utf8Path;
use clove_core::{list_comments, CloveId, GraphStore, Item, ItemFrontmatter};
use serde_json::{json, Map, Value};

/// Whole-store derived state computed once and shared across items in a response.
pub struct GraphContext {
    ready: HashSet<CloveId>,
    /// id → (open hard-dep targets, dangling hard-dep targets) for blocked items.
    blocked: HashMap<CloveId, (Vec<CloveId>, Vec<CloveId>)>,
    graph: GraphStore,
}

impl GraphContext {
    /// Build the derived state from the whole store's frontmatter.
    pub fn build(frontmatters: &[ItemFrontmatter]) -> Self {
        let (graph, _dangling) = GraphStore::build(frontmatters);
        let ready: HashSet<CloveId> = graph.ready_items().into_iter().collect();
        let blocked: HashMap<CloveId, (Vec<CloveId>, Vec<CloveId>)> = graph
            .blocked_items()
            .into_iter()
            .map(|b| (b.id, (b.blocking_deps, b.dangling_deps)))
            .collect();
        Self {
            ready,
            blocked,
            graph,
        }
    }

    /// The underlying graph (for dep trees, cycles, epic rollups).
    pub fn graph(&self) -> &GraphStore {
        &self.graph
    }

    /// Whether `id` is ready to work on now.
    pub fn is_ready(&self, id: &CloveId) -> bool {
        self.ready.contains(id)
    }

    /// Whether `id` is in the blocked partition (an open hard dep or dangling dep).
    pub fn is_blocked(&self, id: &CloveId) -> bool {
        self.blocked.contains_key(id)
    }
}

/// The base item object: serialized frontmatter + `ready`/`blocked_by`/`dangling_deps`.
pub fn frontmatter_value(fm: &ItemFrontmatter, ctx: &GraphContext) -> Map<String, Value> {
    let mut obj = match serde_json::to_value(fm) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    let id = &fm.id;
    obj.insert("ready".to_owned(), json!(ctx.is_ready(id)));

    // `blocked_by` combines open and dangling hard-dep targets (as `clove show`
    // does); `dangling_deps` is the dangling subset alone.
    let (blocked_by, dangling): (Vec<String>, Vec<String>) = match ctx.blocked.get(id) {
        Some((blocking, dang)) => {
            let combined = blocking
                .iter()
                .chain(dang.iter())
                .map(CloveId::to_string)
                .collect();
            (combined, dang.iter().map(CloveId::to_string).collect())
        }
        None => (Vec::new(), Vec::new()),
    };
    obj.insert("blocked_by".to_owned(), json!(blocked_by));
    obj.insert("dangling_deps".to_owned(), json!(dangling));
    obj
}

/// The full detail object: [`frontmatter_value`] plus `body` and `comment_count`.
pub fn item_value(item: &Item, issues_dir: &Utf8Path, ctx: &GraphContext) -> Map<String, Value> {
    let mut obj = frontmatter_value(&item.frontmatter, ctx);
    obj.insert("body".to_owned(), json!(item.body));
    let comment_count = list_comments(issues_dir, &item.frontmatter.id)
        .map(|c| c.len())
        .unwrap_or(0);
    obj.insert("comment_count".to_owned(), json!(comment_count));
    obj
}
