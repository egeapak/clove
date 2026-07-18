//! The store-derived data layer: the file-store scan result and the graph.
//!
//! The scan is factored into a **pure** [`Data::compute`] (a `&ItemStore` in, a
//! [`ScanResult`] out — no `&mut self`, so it runs on a worker thread) and
//! [`Data::apply`] (swap the result into place on the UI thread). [`Data::scan`]
//! chains them for the synchronous callers (launch, post-write refresh); the
//! manual `r` refresh runs `compute` off-thread and delivers the `ScanResult`
//! over a channel — see `App::start_refresh`.

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

/// The wholesale result of a store scan — everything [`Data`] derives from disk.
/// Owned + `Send`, so it can be computed on a worker thread and shipped to the UI
/// thread over a channel.
pub struct ScanResult {
    pub all: Vec<ItemFrontmatter>,
    pub ready: HashSet<CloveId>,
    pub blocked: HashMap<CloveId, BlockedItem>,
    pub graph: GraphStore,
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

    /// Scan `store` and build all derived state, **without touching `self`** — a
    /// pure function of the on-disk files, safe to run on a background thread.
    /// Returns `Err` with a human message if the scan itself failed.
    pub fn compute(store: &ItemStore) -> Result<ScanResult, String> {
        let (mut frontmatters, errors) = store
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

        let ready = graph.ready_items().into_iter().collect();
        let blocked = graph
            .blocked_items()
            .into_iter()
            .map(|b| (b.id.clone(), b))
            .collect();
        let load_warnings = errors.iter().map(|e| e.to_string()).collect();
        Ok(ScanResult {
            all: frontmatters,
            ready,
            blocked,
            graph,
            load_warnings,
        })
    }

    /// Swap a computed [`ScanResult`] into place (UI thread).
    pub fn apply(&mut self, result: ScanResult) {
        self.all = result.all;
        self.ready = result.ready;
        self.blocked = result.blocked;
        self.graph = result.graph;
        self.load_warnings = result.load_warnings;
    }

    /// Re-scan the store synchronously and rebuild the derived graph state.
    /// Returns `Err` with a human message if the scan failed.
    pub fn scan(&mut self) -> Result<(), String> {
        let result = Self::compute(&self.store)?;
        self.apply(result);
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
