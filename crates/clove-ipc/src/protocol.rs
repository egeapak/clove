//! The clove daemon IPC payload types (DESIGN §8.4).
//!
//! These are the request/response *payloads* of the [`crate::service::CloveRpc`]
//! tarpc service — the single source of truth shared by the daemon (`cloved`,
//! server) and the clients ([`crate::client`], the MCP shim), so the wire format
//! can never drift. The service contract itself (method set + the `RpcError`
//! type) lives in [`crate::service`].

use clove_core::graph::DepTreeNode;
use clove_types::{ItemStatus, ItemType, Priority};
use serde::{Deserialize, Serialize};

/// Wire-protocol version, returned by `ping` so a client can detect a daemon
/// built against an incompatible protocol. Bumped on any incompatible change.
///
/// v3 (M4 add/edit): added the `apply_edit(EditRequest)`, `dep_remove`, and
/// `set_parent` mutation methods to the service.
pub const PROTOCOL_VERSION: u32 = 3;

/// A dependency-graph query (DESIGN §8.4 extension for `blocked`/`dep`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GraphRequest {
    /// Active items blocked by open or (with `include_warnings`) missing deps,
    /// in `(priority, topological_rank, id)` order.
    Blocked { include_warnings: bool },
    /// All hard-dependency cycles.
    Cycles,
    /// The dependency tree rooted at `root`, to `depth` (use `usize::MAX` for full).
    Tree { root: String, depth: usize },
    /// Whether adding `from → to` would create a cycle.
    WouldCycle { from: String, to: String },
}

/// The reply to a [`GraphRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "graph", rename_all = "snake_case")]
pub enum GraphResponse {
    /// Ordered blocked-item ids (the CLI reads those files for full detail).
    Blocked { ids: Vec<String> },
    /// Each cycle as its member ids.
    Cycles { cycles: Vec<Vec<String>> },
    /// The dependency tree, or `None` if the root is unknown.
    Tree { node: Option<DepTreeNode> },
    /// Whether the edge would create a cycle.
    WouldCycle { would: bool },
}

/// The payload of a `search` call (the FTS query the daemon runs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    /// The free-text query.
    pub text: String,
    /// Optional result cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Which lean list a `query` call runs — mirrors `clove_index::QueryMode`. Both
/// `clove ls` and `clove query` are [`QueryKind::List`]; `clove ready` is
/// [`QueryKind::Ready`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    /// Status/type/priority/label/assignee filter over all items.
    List,
    /// Unblocked open/in_progress items.
    Ready,
}

/// The payload of a `query` call: the filter the daemon turns into a
/// `clove_index::Filter`. Carries typed model values so the daemon and clients
/// agree without string round-tripping (DESIGN §8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRequest {
    /// Which lean list this is.
    pub kind: QueryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ItemStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_type: Option<ItemType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Page offset (`--offset`).
    pub offset: usize,
    /// Page cap (`--limit`); `None` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// One lean list row on the wire — the columns `clove ls` renders
/// (`{ id, status, type, priority, title }`). Mirrors `clove_index::ItemListRow`
/// without coupling this crate to the index layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanRow {
    pub id: String,
    pub status: String,
    pub item_type: String,
    pub priority: u8,
    pub title: String,
}

/// `query` reply: the (page-limited) lean rows, the full unpaginated match count,
/// and any warnings. Clients shape these with their own list renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryListResponse {
    pub rows: Vec<LeanRow>,
    pub total: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// `reindex` reply (DESIGN §8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReindexDone {
    pub items_indexed: u64,
    pub duration_ms: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// `status` reply: the daemon's operational telemetry (DESIGN §8.4). This is the
/// daemon's *own* runtime state, not work-item analytics (that is `clove stats`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResponse {
    /// Seconds since the daemon became ready.
    pub uptime_s: u64,
    /// Items currently in the index.
    pub items_indexed: u64,
    /// Watcher state, e.g. `"watching"` / `"sweeping"` / `"idle"`.
    pub watcher_state: String,
    /// Milliseconds since the last watcher/IPC event, or `None` if none yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_ms: Option<u64>,
    /// Count of debounced watcher batches applied (feedback-loop / debounce
    /// observable; M3-G05/G06).
    #[serde(default)]
    pub batches_applied: u64,
    /// Total `ping` calls served since startup (heartbeats from clients/the MCP
    /// shim + liveness probes). A health/liveness observable (M4).
    #[serde(default)]
    pub ping_count: u64,
    /// Milliseconds since the last `ping`, or `None` if none yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ping_ms: Option<u64>,
    /// The address the daemon serves the web UI on (`host:port`), if enabled.
    /// Lets `clove serve` detect a serving daemon and hand off instead of binding
    /// its own server (M4 web UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_addr: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every graph request/response payload round-trips through JSON unchanged.
    #[test]
    fn graph_payloads_round_trip() {
        let reqs = vec![
            GraphRequest::Blocked {
                include_warnings: true,
            },
            GraphRequest::Cycles,
            GraphRequest::Tree {
                root: "proj-7af".to_owned(),
                depth: 5,
            },
            GraphRequest::WouldCycle {
                from: "proj-7af".to_owned(),
                to: "proj-3k2".to_owned(),
            },
        ];
        for case in reqs {
            let json = serde_json::to_string(&case).unwrap();
            assert_eq!(case, serde_json::from_str(&json).unwrap(), "{json}");
        }

        let resps = vec![
            GraphResponse::Blocked {
                ids: vec!["proj-7af".to_owned()],
            },
            GraphResponse::Cycles {
                cycles: vec![vec!["proj-a".to_owned(), "proj-b".to_owned()]],
            },
            GraphResponse::WouldCycle { would: true },
            GraphResponse::Tree { node: None },
        ];
        for case in resps {
            let json = serde_json::to_string(&case).unwrap();
            assert_eq!(case, serde_json::from_str(&json).unwrap(), "{json}");
        }
    }

    /// Query/list/status payloads round-trip, including the `None`/empty edges.
    #[test]
    fn list_payloads_round_trip() {
        let cases = vec![
            QueryRequest {
                kind: QueryKind::List,
                status: Some(ItemStatus::Open),
                item_type: Some(ItemType::Bug),
                priority: None,
                assignee: Some("alice".to_owned()),
                label: Some("area:core".to_owned()),
                offset: 0,
                limit: Some(100),
            },
            QueryRequest {
                kind: QueryKind::Ready,
                status: None,
                item_type: None,
                priority: None,
                assignee: None,
                label: None,
                offset: 20,
                limit: None,
            },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            assert_eq!(case, serde_json::from_str(&json).unwrap(), "{json}");
        }

        let resp = QueryListResponse {
            rows: vec![LeanRow {
                id: "proj-7af".to_owned(),
                status: "open".to_owned(),
                item_type: "feature".to_owned(),
                priority: 1,
                title: "do the thing".to_owned(),
            }],
            total: 1,
            warnings: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(resp, serde_json::from_str(&json).unwrap());

        let status = StatusResponse {
            uptime_s: 10,
            items_indexed: 7,
            watcher_state: "watching".to_owned(),
            last_event_ms: Some(1200),
            batches_applied: 3,
            ping_count: 12,
            last_ping_ms: Some(800),
            web_addr: Some("127.0.0.1:7373".to_owned()),
        };
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(status, serde_json::from_str(&json).unwrap());
    }

    /// Edge: an empty search query and an absent limit still round-trip.
    #[test]
    fn search_request_edges() {
        let cases = vec![
            SearchRequest {
                text: String::new(),
                limit: None,
            },
            SearchRequest {
                text: "hello world".to_owned(),
                limit: Some(0),
            },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            assert_eq!(case, serde_json::from_str(&json).unwrap(), "{json}");
        }
    }
}
