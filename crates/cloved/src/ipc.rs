//! IPC server: the daemon's implementation of the `clove-ipc` tarpc service
//! (DESIGN §8.4).
//!
//! `PING`/`STATUS` answer from daemon state; `QUERY` runs the lean `clove_index`
//! list (freshening first, like the CLI's index path) and returns rows the client
//! shapes itself; `SEARCH` runs FTS; `GRAPH` serves the cached graph; `REINDEX`
//! rebuilds and reopens the index. The transport (tarpc over a local socket) is
//! wired in `lifecycle::accept_loop`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use camino::Utf8PathBuf;
use clove_core::view::Order;
use clove_core::ItemStore;
use clove_index::{Filter, Index, ItemListRow, PostFilter, QueryMode};
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
    ///
    /// Dispatched per variant rather than inside one `with_graph` closure,
    /// because `blocked` needs the index as well and the arms must stay
    /// exhaustive — a catch-all `unreachable!()` in an RPC handler turns a future
    /// variant into a daemon panic instead of a compile error.
    fn handle_graph(&self, req: GraphRequest) -> Result<GraphResponse, RpcError> {
        let unreadable = || RpcError::new("graph_failed", "could not read index");
        match req {
            // Two steps: the *set* comes from the graph, but the *order* comes
            // from the index, which is the only place `created`/`updated` and
            // the lifecycle order live (the cached graph's `ItemMeta` carries
            // neither timestamp). Reading the id set out of `with_graph` first
            // also keeps the documented graph→index lock order without nesting
            // the two.
            GraphRequest::Blocked { order } => {
                let blocked: Vec<CloveId> = self
                    .graph
                    .with_graph(|graph, _ranks| {
                        graph
                            .blocked_items()
                            .into_iter()
                            .map(|b| b.id)
                            .collect::<Vec<_>>()
                    })
                    .ok_or_else(unreadable)?;
                Ok(GraphResponse::Blocked {
                    ids: self.order_ids(&blocked, order)?,
                })
            }
            GraphRequest::Cycles => self
                .graph
                .with_graph(|graph, _ranks| GraphResponse::Cycles {
                    cycles: graph
                        .all_cycles()
                        .iter()
                        .map(|c| c.iter().map(|id| id.to_string()).collect())
                        .collect(),
                })
                .ok_or_else(unreadable),
            GraphRequest::Tree { root, depth } => self
                .graph
                .with_graph(|graph, _ranks| GraphResponse::Tree {
                    node: CloveId::new(&root)
                        .ok()
                        .and_then(|id| graph.dep_tree(&id, depth)),
                })
                .ok_or_else(unreadable),
            GraphRequest::WouldCycle { from, to } => self
                .graph
                .with_graph(|graph, _ranks| GraphResponse::WouldCycle {
                    would: match (CloveId::new(&from), CloveId::new(&to)) {
                        (Ok(f), Ok(t)) => graph.check_would_cycle(&f, &t),
                        _ => false,
                    },
                })
                .ok_or_else(unreadable),
        }
    }

    /// Put `ids` into `order`, using the index's `ORDER BY` as the comparator.
    ///
    /// The index is asked for **every** id in the requested order and the subset
    /// is retained from that sequence, rather than sorting the subset directly.
    /// That is deliberate: it reuses `clove_index::query::order_by_sql` — the
    /// same clause `ls`/`ready` run and the same one `sort_order.rs` already
    /// pins against the file path — so `blocked` cannot become a third
    /// implementation of the comparator. It is one indexed scan of a table the
    /// daemon holds open, which is cheaper than the whole-store file read the
    /// CLI is avoiding by asking at all.
    fn order_ids(&self, ids: &[CloveId], order: Order) -> Result<Vec<String>, RpcError> {
        let wanted: HashSet<&str> = ids.iter().map(CloveId::as_str).collect();
        let mut index = self
            .index
            .lock()
            .map_err(|_| RpcError::new("internal", "index lock poisoned"))?;
        self.refresh(&mut index);
        let rows = index
            .query_list(&Filter {
                mode: QueryMode::List,
                order,
                ..Default::default()
            })
            .map_err(|e| RpcError::new("graph_failed", e.to_string()))?;
        Ok(rows
            .into_iter()
            .filter(|row| wanted.contains(row.id.as_str()))
            .map(|row| row.id.to_string())
            .collect())
    }

    /// Run an FTS search over the hot index (freshening first) and return matched
    /// ids in rank order; the client reads those files for full detail.
    fn handle_search(&self, req: SearchRequest) -> Result<Vec<String>, RpcError> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| RpcError::new("internal", "index lock poisoned"))?;
        self.refresh(&mut index);
        // The FTS is a candidate prefilter; the client re-ranks (relevance, or an
        // explicit `--sort`) over the full items, so the SQL order here only
        // decides which rows an explicit `limit` would keep.
        index
            .search(&req.text, &clove_core::view::Order::default(), req.limit)
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

        let (filter, residue) = build_filter(&q);
        // The wire values go through the shared `Page` so a client that is not
        // the `clove` CLI gets the documented contract rather than a raw
        // pass-through: `limit: Some(0)` means *unlimited* here as it does
        // everywhere else, not "zero rows". `query_filtered` owns the rest of
        // the limit/count decision, which a residue changes.
        let window = clove_core::view::Page::new(q.offset, q.limit, 0);
        let (rows, total) = index
            .query_filtered(&filter, residue.as_ref(), window)
            .map_err(|e| RpcError::new("query_failed", e.to_string()))?;
        let total = total as u64;

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

/// Split the wire request's filter set into the SQL half and the in-memory
/// residue, through the *same* [`clove_index::push_down`] the CLI's own index
/// path uses — so the daemon cannot answer a filter differently from the index
/// tier it is standing in for.
///
/// This used to unpack five scalar fields by hand, which is precisely where a
/// newly-added filter would have gone missing.
fn build_filter(q: &QueryRequest) -> (Filter, Option<PostFilter>) {
    let (mut filter, residue) = clove_index::push_down(&q.filters);
    filter.mode = match q.kind {
        QueryKind::List => QueryMode::List,
        QueryKind::Ready => QueryMode::Ready,
    };
    // The requested ordering, carried over the wire. Without this the daemon
    // answered every query in `rank` order while the client's `_meta.sort`
    // claimed otherwise — and, because the SQL `LIMIT` is `offset + limit`, a
    // sorted page would have been the wrong *rows*, not merely the wrong order.
    filter.order = q.order;
    (filter, residue)
}

/// The current time for daemon-side writes (the store truncates to seconds).
fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

/// Map a core error to a wire [`RpcError`], using the *shared* classification
/// from [`clove_types::error_code`] rather than a daemon-local code set.
///
/// The previous hand-rolled table diverged from that taxonomy in two ways that
/// mattered: it used its own spellings (`not_found` vs `ITEM_NOT_FOUND`), and it
/// collapsed everything unrecognized into `op_failed` — merging e.g. `Io`
/// (exit 5) with `ScanFailed` (exit 4), so no client could recover the right
/// exit code. Emitting `(code, exit)` from the one classifier means a failure
/// reported over IPC is indistinguishable from the same failure raised locally.
fn rpc_err(e: CloveError) -> RpcError {
    let (code, exit) = clove_types::error_code(&e);
    RpcError::with_exit(code, e.to_string(), exit)
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
