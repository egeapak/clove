//! The store-derived data layer: the file-store scan result and the graph.
//!
//! Refreshed wholesale by [`Data::scan`]. This is the cohesive state a future
//! concurrent model would put behind its own lock.

use std::collections::{HashMap, HashSet};

use clove_core::{BlockedItem, GraphStore, ItemStore};
use clove_types::{CloveId, ItemFrontmatter};

pub struct Data {
    pub store: ItemStore,
    pub all: Vec<ItemFrontmatter>,
    pub ready: HashSet<CloveId>,
    pub blocked: HashMap<CloveId, BlockedItem>,
    pub graph: GraphStore,
    /// Non-fatal load problems (e.g. files that failed to parse).
    pub load_warnings: Vec<String>,
}

impl Data {
    pub fn new(store: ItemStore) -> Self {
        Data {
            store,
            all: Vec::new(),
            ready: HashSet::new(),
            blocked: HashMap::new(),
            graph: GraphStore::build(&[]).0,
            load_warnings: Vec::new(),
        }
    }

    /// Re-scan the store and rebuild the derived graph state. Returns `Err` with
    /// a human message if the scan itself failed (caller surfaces it as status).
    pub fn scan(&mut self) -> Result<(), String> {
        let (mut frontmatters, errors) = self
            .store
            .scan_frontmatter()
            .map_err(|e| format!("scan failed: {e}"))?;

        let (graph, _dangling) = GraphStore::build(&frontmatters);
        let ranks = graph.topological_ranks();
        frontmatters.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| {
                    let ra = ranks.get(&a.id).copied().unwrap_or(usize::MAX);
                    let rb = ranks.get(&b.id).copied().unwrap_or(usize::MAX);
                    ra.cmp(&rb)
                })
                .then_with(|| a.id.cmp(&b.id))
        });

        self.ready = graph.ready_items().into_iter().collect();
        self.blocked = graph
            .blocked_items()
            .into_iter()
            .map(|b| (b.id.clone(), b))
            .collect();
        self.all = frontmatters;
        self.graph = graph;
        self.load_warnings = errors.iter().map(|e| e.to_string()).collect();
        Ok(())
    }

    pub fn is_ready(&self, id: &CloveId) -> bool {
        self.ready.contains(id)
    }

    pub fn is_blocked(&self, id: &CloveId) -> bool {
        self.blocked.contains_key(id)
    }

    pub fn total(&self) -> usize {
        self.all.len()
    }
}
