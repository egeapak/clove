//! The daemon's cached dependency graph (Tier 2 of CLI→daemon deferral).
//!
//! `blocked`, `dep tree`, `dep cycle`, and the `dep add` cycle pre-check are all
//! graph queries that otherwise cost the CLI a full file scan + `GraphStore`
//! build on every call. The daemon builds the graph once and caches it,
//! rebuilding only when the watcher marks it dirty.
//!
//! **M4 (P3):** the rebuild now reads the graph from the **index database**
//! (`Index::graph_frontmatters` over the `items`/`edges` tables) instead of
//! re-scanning and re-parsing every `.clove/issues/*.md` file. The watcher keeps
//! the index exact and fresh before marking the cache dirty (the incremental
//! `apply_staleness` recomputes the derived columns in-transaction), so a
//! DB-sourced rebuild is graph-equivalent to the file scan it replaces — at a
//! fraction of the cost (two indexed table scans vs. thousands of file opens +
//! YAML parses). Result parity with the CLI's file-scan path is preserved because
//! the index is an exact mirror of the files.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use clove_core::GraphStore;
use clove_index::Index;
use clove_types::CloveId;

/// A built graph plus its topological ranks (for the `blocked` ordering).
struct Built {
    graph: GraphStore,
    ranks: HashMap<CloveId, usize>,
}

/// Lazily-built, watcher-invalidated dependency graph, sourced from the index DB.
pub struct GraphCache {
    index: Arc<Mutex<Index>>,
    built: Mutex<Option<Built>>,
    dirty: AtomicBool,
    /// Monotonic counter bumped on every `mark_dirty` — i.e. on every
    /// graph-affecting change. Exposed over IPC so the MCP server can detect
    /// changes and push `resources/updated` without re-reading the whole graph.
    change_generation: AtomicU64,
}

impl GraphCache {
    pub fn new(index: Arc<Mutex<Index>>) -> GraphCache {
        GraphCache {
            index,
            built: Mutex::new(None),
            dirty: AtomicBool::new(true),
            change_generation: AtomicU64::new(0),
        }
    }

    /// Mark the cache stale (called after each watcher batch + startup sweep).
    /// Also bumps the change-generation counter — this is the single chokepoint
    /// through which every real graph change flows.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
        self.change_generation.fetch_add(1, Ordering::Relaxed);
    }

    /// The current change-generation (a lock-free atomic load).
    pub fn change_generation(&self) -> u64 {
        self.change_generation.load(Ordering::Relaxed)
    }

    /// Run `f` against the current graph, rebuilding first if dirty/empty.
    /// Returns `None` only if the index could not be read.
    ///
    /// Lock order is **graph → index** (the `built` lock is taken first, then the
    /// index lock for the duration of a rebuild). No other path holds the index
    /// lock across a `with_graph` call, so the reverse order never occurs and the
    /// two locks cannot deadlock.
    pub fn with_graph<R>(
        &self,
        f: impl FnOnce(&GraphStore, &HashMap<CloveId, usize>) -> R,
    ) -> Option<R> {
        let mut built = self.built.lock().ok()?;
        // Rebuild when never built or invalidated since the last build. `swap`
        // clears the flag; a concurrent `mark_dirty` during the build just
        // triggers one more rebuild on the next call (never a missed update).
        if built.is_none() || self.dirty.swap(false, Ordering::Relaxed) {
            let frontmatters = {
                let index = self.index.lock().ok()?;
                index.graph_frontmatters().ok()?
            };
            let (graph, _dangling) = GraphStore::build(&frontmatters);
            let ranks = graph.topological_ranks();
            *built = Some(Built { graph, ranks });
        }
        let b = built.as_ref()?;
        Some(f(&b.graph, &b.ranks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clove_index::Index;

    fn cache() -> GraphCache {
        let tmp = tempfile::tempdir().unwrap();
        let db = camino::Utf8PathBuf::from_path_buf(tmp.path().join("index.db")).unwrap();
        let index = Index::open_or_create(&db).unwrap();
        // Leak the tempdir so the db file outlives the test (fine for a unit test).
        std::mem::forget(tmp);
        GraphCache::new(Arc::new(Mutex::new(index)))
    }

    #[test]
    fn change_generation_starts_at_zero_and_bumps_on_mark_dirty() {
        let graph = cache();
        assert_eq!(graph.change_generation(), 0, "fresh cache starts at 0");
        graph.mark_dirty();
        assert_eq!(graph.change_generation(), 1);
        graph.mark_dirty();
        assert_eq!(graph.change_generation(), 2, "each mark_dirty bumps by one");
    }

    #[test]
    fn rebuilding_the_graph_does_not_bump_the_generation() {
        let graph = cache();
        graph.mark_dirty(); // gen = 1, dirty = true
                            // A rebuild (with_graph) reads the graph but must not touch the counter.
        let ran = graph.with_graph(|_g, ranks| ranks.len()).unwrap();
        assert_eq!(ran, 0, "empty index → empty graph");
        assert_eq!(
            graph.change_generation(),
            1,
            "a rebuild is not a change — only mark_dirty bumps the generation"
        );
    }
}
