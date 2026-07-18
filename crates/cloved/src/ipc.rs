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
use clove_core::ItemStore;
use clove_index::{Filter, Index, ItemListRow, QueryMode};
use clove_ipc::{
    CloveRpc, GraphRequest, GraphResponse, LeanRow, QueryKind, QueryListResponse, QueryRequest,
    ReindexDone, RpcError, SearchRequest, StatusResponse, PROTOCOL_VERSION,
};
use clove_types::{CloveError, CloveId, ItemType, NewSpec};
use serde_json::Value;
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
    pub repo_root: Utf8PathBuf,
    pub issues_dir: Utf8PathBuf,
    pub db_path: Utf8PathBuf,
    pub auto_refresh: bool,
    pub graph: Arc<GraphCache>,
    /// Id prefix + default type for daemon-side `create` (from `.clove/config`).
    pub id_prefix: String,
    pub default_type: ItemType,
}

impl CloveRpc for Dispatcher {
    async fn ping(self, _: Context) -> u32 {
        // A ping is a heartbeat: count it and reset the idle-shutdown window.
        if let Ok(mut state) = self.state.lock() {
            state.record_ping();
        }
        PROTOCOL_VERSION
    }

    async fn change_generation(self, _: Context) -> u64 {
        // Cheap lock-free read; also a heartbeat so an active MCP notify-poll
        // keeps the daemon's idle-shutdown window reset (like `ping`).
        self.touch();
        self.graph.change_generation()
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
                ping_count: 0,
                last_ping_ms: None,
                web_addr: None,
            },
        }
    }

    async fn query(self, _: Context, req: QueryRequest) -> Result<QueryListResponse, RpcError> {
        self.touch();
        self.blocking(move |this| this.handle_query(req)).await
    }

    async fn search(self, _: Context, req: SearchRequest) -> Result<Vec<String>, RpcError> {
        self.touch();
        self.blocking(move |this| this.handle_search(req)).await
    }

    async fn graph(self, _: Context, req: GraphRequest) -> Result<GraphResponse, RpcError> {
        self.touch();
        self.blocking(move |this| this.handle_graph(req)).await
    }

    async fn reindex(self, _: Context) -> Result<ReindexDone, RpcError> {
        self.touch();
        self.blocking(|this| this.handle_reindex()).await
    }

    async fn create(self, _: Context, spec: NewSpec) -> Result<Value, RpcError> {
        self.touch();
        self.blocking(move |this| {
            let out = clove_core::ops::create(
                &this.store(),
                &this.id_prefix,
                this.default_type,
                spec,
                now(),
            )
            .map_err(rpc_err);
            this.after_write(&out);
            out
        })
        .await
    }

    async fn set_status(
        self,
        _: Context,
        id: String,
        status: clove_types::ItemStatus,
    ) -> Result<Value, RpcError> {
        self.touch();
        let cid = CloveId::new(&id).map_err(rpc_err)?;
        self.blocking(move |this| {
            let out =
                clove_core::ops::transition(&this.store(), &cid, status, now()).map_err(rpc_err);
            this.after_write(&out);
            out
        })
        .await
    }

    async fn edit(
        self,
        _: Context,
        id: String,
        assignments: Vec<String>,
    ) -> Result<Value, RpcError> {
        self.touch();
        let cid = CloveId::new(&id).map_err(rpc_err)?;
        self.blocking(move |this| {
            let out =
                clove_core::ops::edit(&this.store(), &cid, &assignments, now()).map_err(rpc_err);
            this.after_write(&out);
            out
        })
        .await
    }

    async fn apply_edit(
        self,
        _: Context,
        id: String,
        req: clove_types::EditRequest,
    ) -> Result<Value, RpcError> {
        self.touch();
        let cid = CloveId::new(&id).map_err(rpc_err)?;
        self.blocking(move |this| {
            let out = clove_core::apply_edit(&this.store(), &cid, &req, now()).map_err(rpc_err);
            this.after_write(&out);
            out
        })
        .await
    }

    async fn add_comment(
        self,
        _: Context,
        id: String,
        author: String,
        body: String,
    ) -> Result<Value, RpcError> {
        self.touch();
        let cid = CloveId::new(&id).map_err(rpc_err)?;
        self.blocking(move |this| {
            let out =
                clove_core::ops::comment(&this.store(), &cid, &author, &body).map_err(rpc_err);
            this.after_write(&out);
            out
        })
        .await
    }

    async fn dep_add(self, _: Context, id: String, dep_id: String) -> Result<Value, RpcError> {
        self.touch();
        let cid = CloveId::new(&id).map_err(rpc_err)?;
        let dep = CloveId::new(&dep_id).map_err(rpc_err)?;
        self.blocking(move |this| {
            let out = clove_core::ops::dep_add(&this.store(), &cid, &dep, now()).map_err(rpc_err);
            this.after_write(&out);
            out
        })
        .await
    }

    async fn dep_remove(self, _: Context, id: String, dep_id: String) -> Result<Value, RpcError> {
        self.touch();
        let cid = CloveId::new(&id).map_err(rpc_err)?;
        let dep = CloveId::new(&dep_id).map_err(rpc_err)?;
        self.blocking(move |this| {
            let out =
                clove_core::ops::dep_remove(&this.store(), &cid, &dep, now()).map_err(rpc_err);
            this.after_write(&out);
            out
        })
        .await
    }

    async fn set_parent(
        self,
        _: Context,
        id: String,
        parent: Option<String>,
    ) -> Result<Value, RpcError> {
        self.touch();
        let cid = CloveId::new(&id).map_err(rpc_err)?;
        let parent = match parent {
            Some(p) => Some(CloveId::new(&p).map_err(rpc_err)?),
            None => None,
        };
        self.blocking(move |this| {
            let out = clove_core::ops::set_parent(&this.store(), &cid, parent.as_ref(), now())
                .map_err(rpc_err);
            this.after_write(&out);
            out
        })
        .await
    }

    async fn show(self, _: Context, id: String) -> Result<Value, RpcError> {
        self.touch();
        let cid = CloveId::new(&id).map_err(rpc_err)?;
        self.blocking(move |this| clove_core::ops::show(&this.store(), &cid).map_err(rpc_err))
            .await
    }

    async fn stats(self, _: Context, top: u32, include_epics: bool) -> Result<Value, RpcError> {
        self.touch();
        self.blocking(move |this| {
            clove_core::ops::stats(&this.store(), top as usize, include_epics, now())
                .map_err(rpc_err)
        })
        .await
    }
}

impl Dispatcher {
    /// Run blocking store/index work (SQLite queries, `std::sync::Mutex`
    /// acquisition, full-directory scans, reindex) off the async worker threads.
    ///
    /// The daemon's runtime has only 2 workers (DESIGN §8.1) and also hosts the
    /// accept loop, the watcher, the idle watchdog, and the axum web server, so
    /// running blocking handler work inline would let one slow op (e.g. a reindex
    /// holding the index mutex) park a worker and starve `ping`/`status`/the web
    /// UI — which in turn trips the client's 50ms ping budget. Offloading to the
    /// blocking pool keeps the async workers responsive. `Dispatcher` is cheap to
    /// clone (all `Arc`), so the closure owns its own handle.
    async fn blocking<T, F>(&self, f: F) -> Result<T, RpcError>
    where
        F: FnOnce(Dispatcher) -> Result<T, RpcError> + Send + 'static,
        T: Send + 'static,
    {
        let this = self.clone();
        match tokio::task::spawn_blocking(move || f(this)).await {
            Ok(res) => res,
            Err(_) => Err(RpcError::new("internal", "daemon worker task failed")),
        }
    }

    /// Record that an IPC event happened (resets the idle-shutdown window).
    fn touch(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.mark_event();
        }
    }

    /// The file store rooted at the repo (daemon-side writes for topology B).
    fn store(&self) -> ItemStore {
        ItemStore::new(self.repo_root.clone())
    }

    /// After a successful daemon-side write, freshen the index from the
    /// just-changed files and rebuild the hot graph so the daemon's lean
    /// query/graph reads stay coherent (file-based `show`/`stats`/`list` ops are
    /// already correct). A no-op on a failed write.
    fn after_write(&self, result: &Result<Value, RpcError>) {
        if result.is_err() {
            return;
        }
        if let Ok(mut index) = self.index.lock() {
            if let Ok(report) = index.check_staleness_fast(&self.issues_dir) {
                if !report.is_clean() {
                    let _ = index.apply_staleness(&report, &self.issues_dir);
                }
            }
        }
        self.graph.mark_dirty();
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
        // A failed reopen must surface: silently keeping the old handle would
        // leave the daemon serving the *replaced* (unlinked) inode — diverging
        // from the on-disk index.db every other process sees — while telling
        // the client the reindex succeeded.
        let fresh = Index::open_or_create(&self.db_path).map_err(|e| {
            RpcError::new(
                "reindex_reopen_failed",
                format!("index rebuilt but the daemon could not reopen it: {e}"),
            )
        })?;
        *index = fresh;
        if let Ok(mut state) = self.state.lock() {
            state.set_items_indexed(index.item_count().unwrap_or(0) as u64);
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

/// The current time for daemon-side writes (the store truncates to seconds).
fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

/// Map a core error to a wire [`RpcError`] with a stable machine code so clients
/// (the MCP server) can distinguish failure classes.
fn rpc_err(e: CloveError) -> RpcError {
    let code = match &e {
        CloveError::NotFound { .. } => "not_found",
        CloveError::SelfDependency { .. } => "self_loop",
        CloveError::DependencyCycle { .. } => "cycle",
        CloveError::DependencyExists { .. } => "already_exists",
        CloveError::InvalidField { .. } => "invalid_field",
        _ => "op_failed",
    };
    RpcError::new(code, e.to_string())
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
