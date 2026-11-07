//! Item → JSON shaping for the web API.
//!
//! Reuses the on-disk frontmatter's derived `Serialize` (so the shape matches
//! DESIGN §7.4 and the CLI's `clove show --format json`) and augments it with the
//! graph-computed fields `ready`, `blocked_by`, `dangling_deps`, the web-only
//! `missing_refs` — plus `body` and `comment_count` for the detail view. The graph is built once per request and
//! shared across all items via [`GraphContext`].

use std::collections::{HashMap, HashSet};

use camino::Utf8Path;
use clove_core::ops::GraphTerms;
use clove_core::{list_comments, GraphStore, ItemStore};
use clove_types::{CloveId, Item, ItemFrontmatter};
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
        Self::from_graph(graph)
    }

    /// Build the derived state from a graph someone else already constructed.
    ///
    /// `clove_engine`'s file tier returns the graph it built for the topological
    /// ranks, so the web derives `ready`/`blocked_by` from that rather than
    /// scanning the store and building a second one per request.
    pub fn from_graph(graph: GraphStore) -> Self {
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
}

/// The base item object: serialized frontmatter + `ready`/`blocked_by`/`dangling_deps`.
pub fn frontmatter_value(fm: &ItemFrontmatter, ctx: &GraphContext) -> Map<String, Value> {
    let id = &fm.id;
    // `blocked_by` combines open and dangling hard-dep targets (as `clove show`
    // does); `dangling_deps` is the dangling subset alone.
    let (blocked_by, dangling_deps): (Vec<String>, Vec<String>) = match ctx.blocked.get(id) {
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
    let missing = missing_refs(fm, |r| !ctx.graph.dangling_ids().contains(r));
    with_terms(
        fm,
        &GraphTerms {
            ready: ctx.is_ready(id),
            blocked_by,
            dangling_deps,
        },
        missing,
    )
}

/// Every id that `fm` references — `deps`, `parent`, `relates`, `duplicates`,
/// `supersedes` — with no backing item, whatever the item's status. Deduped, in
/// that field order.
///
/// `dangling_deps` cannot serve here: it is the dangling subset of `blocked_by`,
/// so a closed or cycle-excluded item reports none, and it covers hard deps
/// only. The SPA reads `missing_refs` to show a reference as missing without
/// requesting it (each request would answer 404).
pub fn missing_refs(fm: &ItemFrontmatter, exists: impl Fn(&CloveId) -> bool) -> Vec<String> {
    let mut seen: HashSet<&CloveId> = HashSet::new();
    fm.deps
        .iter()
        .chain(fm.parent.iter())
        .chain(fm.relates.iter())
        .chain(fm.duplicates.iter())
        .chain(fm.supersedes.iter())
        .filter(|r| seen.insert(*r) && !exists(r))
        .map(CloveId::to_string)
        .collect()
}

/// [`missing_refs`] checked against the files, for an answer with no whole-store
/// graph. An id counts as present the way the store's scan counts it — a regular
/// file, not a symlink (DESIGN §12.3), whose frontmatter parses — which is the
/// rule `ops::graph_terms_detailed` uses too, so a tiered row agrees with the
/// file tier's.
pub fn missing_refs_on_disk(store: &ItemStore, fm: &ItemFrontmatter) -> Vec<String> {
    missing_refs(fm, |r| {
        let path = store.path_for(r);
        std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file())
            && clove_core::parse_frontmatter_file(&path).is_ok()
    })
}

/// The base item object from per-item graph terms.
///
/// Used when an index or daemon tier answered and there is no whole-store graph
/// to read from: `clove_core::ops::graph_terms_detailed` computes the same three
/// values from the item's own dependency closure, so the two spellings of this
/// object are interchangeable — which is what lets the web keep one row shape
/// across all three tiers.
pub fn with_terms(
    fm: &ItemFrontmatter,
    terms: &GraphTerms,
    missing_refs: Vec<String>,
) -> Map<String, Value> {
    let mut obj = match serde_json::to_value(fm) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    obj.insert("ready".to_owned(), json!(terms.ready));
    obj.insert("blocked_by".to_owned(), json!(terms.blocked_by));
    obj.insert("dangling_deps".to_owned(), json!(terms.dangling_deps));
    obj.insert("missing_refs".to_owned(), json!(missing_refs));
    obj
}

/// The full detail object: [`with_terms`] plus `body` and `comment_count`.
pub fn item_value(
    item: &Item,
    issues_dir: &Utf8Path,
    terms: &GraphTerms,
    missing_refs: Vec<String>,
) -> Map<String, Value> {
    let mut obj = with_terms(&item.frontmatter, terms, missing_refs);
    obj.insert("body".to_owned(), json!(item.body));
    let comment_count = list_comments(issues_dir, &item.frontmatter.id)
        .map(|c| c.len())
        .unwrap_or(0);
    obj.insert("comment_count".to_owned(), json!(comment_count));
    obj
}
