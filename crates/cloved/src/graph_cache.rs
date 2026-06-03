//! The daemon's cached dependency graph (Tier 2 of CLI→daemon deferral).
//!
//! `blocked`, `dep tree`, `dep cycle`, and the `dep add` cycle pre-check are all
//! graph queries that otherwise cost the CLI a full file scan + `GraphStore`
//! build on every call. The daemon builds the graph once (from files, via the
//! same `ItemStore::scan_frontmatter` + `GraphStore::build` the CLI uses, so
//! results are identical) and caches it, rebuilding only when the watcher marks
//! it dirty. Repeated graph queries are then served with no rescan.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use camino::Utf8PathBuf;
use clove_core::{CloveId, GraphStore, ItemStore};

/// A built graph plus its topological ranks (for the `blocked` ordering).
struct Built {
    graph: GraphStore,
    ranks: HashMap<CloveId, usize>,
}

/// Lazily-built, watcher-invalidated dependency graph.
pub struct GraphCache {
    repo_root: Utf8PathBuf,
    built: Mutex<Option<Built>>,
    dirty: AtomicBool,
}

impl GraphCache {
    pub fn new(repo_root: Utf8PathBuf) -> GraphCache {
        GraphCache {
            repo_root,
            built: Mutex::new(None),
            dirty: AtomicBool::new(true),
        }
    }

    /// Mark the cache stale (called after each watcher batch + startup sweep).
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Run `f` against the current graph, rebuilding first if dirty/empty.
    /// Returns `None` only if the files could not be scanned.
    pub fn with_graph<R>(
        &self,
        f: impl FnOnce(&GraphStore, &HashMap<CloveId, usize>) -> R,
    ) -> Option<R> {
        let mut built = self.built.lock().ok()?;
        // Rebuild when never built or invalidated since the last build. `swap`
        // clears the flag; a concurrent `mark_dirty` during the build just
        // triggers one more rebuild on the next call (never a missed update).
        if built.is_none() || self.dirty.swap(false, Ordering::Relaxed) {
            let store = ItemStore::new(self.repo_root.clone());
            let (frontmatters, _errors) = store.scan_frontmatter().ok()?;
            let (graph, _dangling) = GraphStore::build(&frontmatters);
            let ranks = graph.topological_ranks();
            *built = Some(Built { graph, ranks });
        }
        let b = built.as_ref()?;
        Some(f(&b.graph, &b.ranks))
    }
}
