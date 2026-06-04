//! IPC server: the async side of the clove-ipc wire protocol (DESIGN §8.4).
//!
//! The framing matches [`clove_ipc::frame`] (4-byte LE length prefix + JSON), but
//! is driven over Tokio's `AsyncRead`/`AsyncWrite` here so the accept loop stays
//! on the runtime. The blocking client in `clove-ipc` interoperates byte-for-byte.
//!
//! Commands (DESIGN §8.4): `PING`/`STATUS` answer from daemon state; `QUERY` runs
//! the lean `clove_index` list (freshening first, like the CLI's index path) and
//! returns rows the CLI shapes itself; `REINDEX` rebuilds and reopens the index.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use camino::Utf8PathBuf;
use clove_core::CloveId;
use clove_index::{Filter, Index, ItemListRow, QueryMode};
use clove_ipc::frame::MAX_FRAME;
use clove_ipc::protocol::{
    ErrorResponse, GraphRequest, GraphResponse, LeanRow, QueryKind, QueryListResponse,
    QueryRequest, ReindexDone, Request, Response,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::graph_cache::GraphCache;
use crate::state::DaemonState;

/// Above this many out-of-date items, the QUERY-time refresh is skipped (the
/// rows may then be slightly behind until the watcher catches up); mirrors the
/// CLI's `STALE_REFRESH_LIMIT` (DESIGN §6.4).
const STALE_REFRESH_LIMIT: usize = 20;

/// Shared context every connection handler needs.
#[derive(Clone)]
pub struct Dispatcher {
    pub index: Arc<Mutex<Index>>,
    pub state: Arc<Mutex<DaemonState>>,
    pub issues_dir: Utf8PathBuf,
    pub db_path: Utf8PathBuf,
    pub auto_refresh: bool,
    pub graph: Arc<GraphCache>,
}

impl Dispatcher {
    /// Map a request to a response.
    pub fn dispatch(&self, req: Request) -> Response {
        if let Ok(mut state) = self.state.lock() {
            state.mark_event();
        }
        match req {
            Request::Ping => Response::Pong,
            Request::Status => match self.state.lock() {
                Ok(state) => Response::Status(state.snapshot()),
                Err(_) => Response::Error(ErrorResponse::new("internal", "state lock poisoned")),
            },
            Request::Query(q) => self.handle_query(q),
            Request::Search(s) => self.handle_search(s),
            Request::Graph(g) => self.handle_graph(g),
            Request::Reindex => self.handle_reindex(),
        }
    }

    /// Serve a dependency-graph query from the daemon's cached graph (Tier 2).
    fn handle_graph(&self, req: GraphRequest) -> Response {
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
        match resp {
            Some(r) => Response::Graph(r),
            None => Response::Error(ErrorResponse::new("graph_failed", "could not scan files")),
        }
    }

    /// Run an FTS search over the hot index (freshening first) and return matched
    /// ids in rank order; the CLI reads those files for full detail.
    fn handle_search(&self, req: clove_ipc::SearchRequest) -> Response {
        let mut index = match self.index.lock() {
            Ok(g) => g,
            Err(_) => {
                return Response::Error(ErrorResponse::new("internal", "index lock poisoned"))
            }
        };
        if self.auto_refresh {
            if let Ok(report) = index.check_staleness_fast(&self.issues_dir) {
                if !report.is_clean() && report.change_count() <= STALE_REFRESH_LIMIT {
                    let _ = index.apply_staleness(&report, &self.issues_dir);
                }
            }
        }
        match index.search(&req.text, req.limit) {
            Ok(rows) => Response::SearchIds {
                ids: rows.into_iter().map(|r| r.id).collect(),
            },
            Err(e) => Response::Error(ErrorResponse::new("search_failed", e.to_string())),
        }
    }

    /// Serve a lean list query, freshening the index inline first (the daemon owns
    /// freshness; the watcher in P3 makes this a no-op in the steady state).
    fn handle_query(&self, q: QueryRequest) -> Response {
        let mut index = match self.index.lock() {
            Ok(g) => g,
            Err(_) => {
                return Response::Error(ErrorResponse::new("internal", "index lock poisoned"))
            }
        };

        if self.auto_refresh {
            if let Ok(report) = index.check_staleness_fast(&self.issues_dir) {
                if !report.is_clean() && report.change_count() <= STALE_REFRESH_LIMIT {
                    let _ = index.apply_staleness(&report, &self.issues_dir);
                }
            }
        }

        let filter = build_filter(&q);
        let total = match index.count_items(&filter) {
            Ok(n) => n as u64,
            Err(e) => return Response::Error(ErrorResponse::new("query_failed", e.to_string())),
        };
        let rows = match index.query_list(&filter) {
            Ok(rows) => rows,
            Err(e) => return Response::Error(ErrorResponse::new("query_failed", e.to_string())),
        };

        if let Ok(mut state) = self.state.lock() {
            state.set_items_indexed(index.item_count().unwrap_or(0) as u64);
        }

        Response::QueryList(QueryListResponse {
            rows: rows.iter().map(to_lean).collect(),
            total,
            warnings: Vec::new(),
        })
    }

    /// Rebuild the index from files, then reopen so the daemon serves the rebuilt
    /// file rather than the replaced inode.
    fn handle_reindex(&self) -> Response {
        let start = Instant::now();
        let report = match clove_index::reindex(&self.issues_dir, &self.db_path) {
            Ok(report) => report,
            Err(e) => return Response::Error(ErrorResponse::new("reindex_failed", e.to_string())),
        };
        if let (Ok(mut index), Ok(fresh)) =
            (self.index.lock(), Index::open_or_create(&self.db_path))
        {
            *index = fresh;
            if let Ok(mut state) = self.state.lock() {
                state.set_items_indexed(index.item_count().unwrap_or(0) as u64);
            }
        }
        Response::ReindexDone(ReindexDone {
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

/// Read one length-prefixed frame asynchronously. `Ok(None)` means the peer
/// closed the connection cleanly at a frame boundary (clean EOF on the prefix).
pub async fn read_frame_async<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds MAX_FRAME",
        ));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

/// Write one length-prefixed frame asynchronously.
pub async fn write_frame_async<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    let len: u32 = payload
        .len()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame too large"))?;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

/// Serve one connection: read requests, dispatch, write responses, until the peer
/// closes or a transport error occurs. A malformed frame is answered with an error
/// response and the connection is dropped; the daemon stays up.
pub async fn handle_connection<S>(mut stream: S, dispatcher: Dispatcher) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(payload) = read_frame_async(&mut stream).await? {
        let response = match serde_json::from_slice::<Request>(&payload) {
            Ok(req) => dispatcher.dispatch(req),
            Err(e) => Response::Error(ErrorResponse::new("bad_request", e.to_string())),
        };
        let out = serde_json::to_vec(&response)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        write_frame_async(&mut stream, &out).await?;
    }
    Ok(())
}
