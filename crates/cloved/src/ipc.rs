//! IPC server: the daemon's implementation of the `clove-ipc` tarpc service
//! (DESIGN §8.4).
//!
//! `PING`/`STATUS` answer from daemon state; `QUERY` runs the lean `clove_index`
//! list (freshening first, like the CLI's index path) and returns rows the client
//! shapes itself; `SEARCH` runs FTS; `GRAPH` serves the cached graph; `REINDEX`
//! rebuilds and reopens the index. The transport (tarpc over a local socket) is
//! wired in `lifecycle::accept_loop`.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use camino::Utf8PathBuf;
use clove_core::CloveId;
use clove_index::{Filter, Index, ItemListRow, QueryMode};
use clove_ipc::{
    CloveRpc, GraphRequest, GraphResponse, LeanRow, QueryKind, QueryListResponse, QueryRequest,
    ReindexDone, RpcError, SearchRequest, StatusResponse, PROTOCOL_VERSION,
};
use tarpc::context::Context;

use crate::graph_cache::GraphCache;
use crate::state::DaemonState;

/// Above this many out-of-date items, the QUERY-time refresh is skipped (the
/// rows may then be slightly behind until the watcher catches up); mirrors the
/// CLI's `STALE_REFRESH_LIMIT` (DESIGN §6.4).
const STALE_REFRESH_LIMIT: usize = 20;

/// Shared context every connection handler needs. Cloned per request by tarpc,
/// so all fields are cheap (`Arc`) handles.
#[derive(Clone)]
pub struct Dispatcher {
    pub index: Arc<Mutex<Index>>,
    pub state: Arc<Mutex<DaemonState>>,
    pub issues_dir: Utf8PathBuf,
    pub db_path: Utf8PathBuf,
    pub auto_refresh: bool,
    pub graph: Arc<GraphCache>,
}

impl CloveRpc for Dispatcher {
    async fn ping(self, _: Context) -> u32 {
        self.touch();
        PROTOCOL_VERSION
    }

    async fn status(self, _: Context) -> StatusResponse {
        self.touch();
        match self.state.lock() {
            Ok(state) => state.snapshot(),
            Err(_) => StatusResponse {
                uptime_s: 0,
                items_indexed: 0,
                watcher_state: "error".to_owned(),
                last_event_ms: None,
                batches_applied: 0,
            },
        }
    }

    async fn query(self, _: Context, req: QueryRequest) -> Result<QueryListResponse, RpcError> {
        self.touch();
        self.handle_query(req)
    }

    async fn search(self, _: Context, req: SearchRequest) -> Result<Vec<String>, RpcError> {
        self.touch();
        self.handle_search(req)
    }

    async fn graph(self, _: Context, req: GraphRequest) -> Result<GraphResponse, RpcError> {
        self.touch();
        self.handle_graph(req)
    }

    async fn reindex(self, _: Context) -> Result<ReindexDone, RpcError> {
        self.touch();
        self.handle_reindex()
    }
}

impl Dispatcher {
    /// Record that an IPC event happened (resets the idle-shutdown window).
    fn touch(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.mark_event();
        }
    }

    /// Serve a dependency-graph query from the daemon's cached graph (Tier 2).
    fn handle_graph(&self, req: GraphRequest) -> Result<GraphResponse, RpcError> {
        let resp = self.graph.with_graph(|graph, ranks| match req {
            GraphRequest::Cycles => GraphResponse::Cycles {
                cycles: graph
                    .all_cycles()
                    .iter()
                    .map(|c| c.iter().map(|id| id.to_string()).collect())
                    .collect(),
            },
            GraphRequest::Tree { root, depth } => {
                let node = CloveId::new(&root)
                    .ok()
                    .and_then(|id| graph.dep_tree(&id, depth));
                GraphResponse::Tree { node }
            }
            GraphRequest::WouldCycle { from, to } => {
                let would = match (CloveId::new(&from), CloveId::new(&to)) {
                    (Ok(f), Ok(t)) => graph.check_would_cycle(&f, &t),
                    _ => false,
                };
                GraphResponse::WouldCycle { would }
            }
            GraphRequest::Blocked { include_warnings } => {
                // Same set + (priority, topological_rank, id) order as the CLI's
                // file path (`clove blocked`), computed from the graph alone.
                let mut keyed: Vec<(u8, usize, CloveId)> = graph
                    .blocked_items()
                    .into_iter()
                    .filter(|b| include_warnings || !b.blocking_deps.is_empty())
                    .filter_map(|b| {
                        graph.meta(&b.id).map(|m| {
                            let rank = ranks.get(&b.id).copied().unwrap_or(usize::MAX);
                            (m.priority.0, rank, b.id.clone())
                        })
                    })
                    .collect();
                keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
                GraphResponse::Blocked {
                    ids: keyed.into_iter().map(|(_, _, id)| id.to_string()).collect(),
                }
            }
        });
        resp.ok_or_else(|| RpcError::new("graph_failed", "could not read index"))
    }

    /// Run an FTS search over the hot index (freshening first) and return matched
    /// ids in rank order; the client reads those files for full detail.
    fn handle_search(&self, req: SearchRequest) -> Result<Vec<String>, RpcError> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| RpcError::new("internal", "index lock poisoned"))?;
        self.refresh(&mut index);
        index
            .search(&req.text, req.limit)
            .map(|rows| rows.into_iter().map(|r| r.id).collect())
            .map_err(|e| RpcError::new("search_failed", e.to_string()))
    }

    /// Serve a lean list query, freshening the index inline first (the daemon owns
    /// freshness; the watcher in P3 makes this a no-op in the steady state).
    fn handle_query(&self, q: QueryRequest) -> Result<QueryListResponse, RpcError> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| RpcError::new("internal", "index lock poisoned"))?;
        self.refresh(&mut index);

        let filter = build_filter(&q);
        let total = index
            .count_items(&filter)
            .map_err(|e| RpcError::new("query_failed", e.to_string()))? as u64;
        let rows = index
            .query_list(&filter)
            .map_err(|e| RpcError::new("query_failed", e.to_string()))?;

        if let Ok(mut state) = self.state.lock() {
            state.set_items_indexed(index.item_count().unwrap_or(0) as u64);
        }

        Ok(QueryListResponse {
            rows: rows.iter().map(to_lean).collect(),
            total,
            warnings: Vec::new(),
        })
    }

    /// Freshen the hot index from disk if it is lightly stale (shared by the
    /// query and search paths). A heavier drift is left to the watcher.
    fn refresh(&self, index: &mut Index) {
        if !self.auto_refresh {
            return;
        }
        if let Ok(report) = index.check_staleness_fast(&self.issues_dir) {
            if !report.is_clean() && report.change_count() <= STALE_REFRESH_LIMIT {
                let _ = index.apply_staleness(&report, &self.issues_dir);
                // The DB advanced; the hot graph (sourced from it) must rebuild.
                self.graph.mark_dirty();
            }
        }
    }

    /// Rebuild the index from files, then reopen so the daemon serves the rebuilt
    /// file rather than the replaced inode.
    ///
    /// The index lock is held across the whole rebuild + reopen so a concurrent
    /// auto-snapshot (`snapshot_loop`) cannot record into the live database during
    /// the window between `reindex`'s snapshot carry-over and its atomic rename —
    /// which would write to the about-to-be-replaced inode and lose that history
    /// point. Reindex is an explicit, infrequent operation, so briefly serializing
    /// queries behind it is an acceptable trade for not dropping snapshots.
    fn handle_reindex(&self) -> Result<ReindexDone, RpcError> {
        let start = Instant::now();
        let mut index = self
            .index
            .lock()
            .map_err(|_| RpcError::new("internal", "index lock poisoned"))?;
        let report = clove_index::reindex(&self.issues_dir, &self.db_path)
            .map_err(|e| RpcError::new("reindex_failed", e.to_string()))?;
        if let Ok(fresh) = Index::open_or_create(&self.db_path) {
            *index = fresh;
            if let Ok(mut state) = self.state.lock() {
                state.set_items_indexed(index.item_count().unwrap_or(0) as u64);
            }
        }
        drop(index);
        // The index was rebuilt and reopened; rebuild the hot graph from it.
        self.graph.mark_dirty();
        Ok(ReindexDone {
            items_indexed: report.items_indexed as u64,
            duration_ms: start.elapsed().as_millis() as u64,
            warnings: report.warnings,
        })
    }
}

/// Build a `clove_index::Filter` from the wire request (mirrors the CLI's
/// `list_via_index`: fetch `offset + limit` rows; `total` is reported separately).
fn build_filter(q: &QueryRequest) -> Filter {
    Filter {
        mode: match q.kind {
            QueryKind::List => QueryMode::List,
            QueryKind::Ready => QueryMode::Ready,
        },
        status: q.status.map(|s| vec![s]),
        item_type: q.item_type,
        priority: q.priority,
        assignee: q.assignee.clone(),
        label: q.label.clone(),
        parent: None,
        limit: q.limit.map(|n| q.offset.saturating_add(n)),
    }
}

/// Project an index row onto the lean wire row.
fn to_lean(row: &ItemListRow) -> LeanRow {
    LeanRow {
        id: row.id.as_str().to_owned(),
        status: row.status.as_str().to_owned(),
        item_type: row.item_type.as_str().to_owned(),
        priority: row.priority,
        title: row.title.clone(),
    }
}
