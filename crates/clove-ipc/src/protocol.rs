//! The clove daemon IPC payload types (DESIGN §8.4).
//!
//! These are the request/response *payloads* of the [`crate::service::CloveRpc`]
//! tarpc service — the single source of truth shared by the daemon (`cloved`,
//! server) and the clients ([`crate::client`], the MCP shim), so the wire format
//! can never drift. The service contract itself (method set + the `RpcError`
//! type) lives in [`crate::service`].

use clove_core::graph::DepTreeNode;
use clove_core::view::Order;
use serde::{Deserialize, Serialize};

/// Wire-protocol version, returned by `ping` so a client can detect a daemon
/// built against an incompatible protocol. Bumped on any incompatible change.
///
/// v3 (M4 add/edit): added the `apply_edit(EditRequest)`, `dep_remove`, and
/// `set_parent` mutation methods to the service.
///
/// v4 (MCP resource-push): added the `change_generation()` query, letting the
/// MCP server poll the cache's monotonic change counter to emit
/// `resources/updated` notifications when the work graph mutates.
///
/// v5 (read-path §2, shared filters): `QueryRequest`'s five scalar filter fields
/// collapsed into one `filters: clove_core::view::Filters`, whose status / type
/// / priority are now **sets** and whose labels are AND-ed, plus a `q`
/// substring. The codec is length-delimited JSON, so a mixed-version pair would
/// still *decode* — which is exactly why this needs a version: a v4 client's
/// `"status": "open"` silently drops against a v5 daemon (wrong field, wrong
/// shape), and a v4 daemon ignores a v5 client's whole filter set. Both answer
/// with the unfiltered list rather than an error. The handshake in `client.rs`
/// turns that into a clean mismatch instead; `clove daemon` restarts are cheap
/// and the daemon is a cache, not a source of truth.
///
/// The same bump removes the dead `GraphRequest::Blocked::include_warnings`
/// (unreachable — no surface could set it, every caller hard-coded `true`) and
/// gives `Blocked` the `order` it always needed.
///
/// **v6 removes the `search` RPC and `SearchRequest`.** The daemon answered
/// search by running the index's FTS5 query and returning matched ids; index
/// schema 6 deleted that table, because FTS matched whole ASCII-folded tokens
/// where `clove_core::view::match_class` matches Unicode substrings, so
/// `clove search X` returned different ids depending on whether a daemon or an
/// index happened to be present (read-path roadmap §6.1). Search is now a
/// parallel file scan on every surface, which the client does for itself — the
/// daemon had nothing left to contribute, since the client had to read every
/// matched file anyway to rank it. Removing the method is what makes the
/// handshake reject a v5 daemon rather than let it keep answering searches with
/// the old, narrower match set.
pub const PROTOCOL_VERSION: u32 = 6;

/// A dependency-graph query (DESIGN §8.4 extension for `blocked`/`dep`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GraphRequest {
    /// Active items blocked by open or missing dependencies, in `order`.
    ///
    /// Ordering rides the request because the alternative was the client
    /// re-sorting the returned ids — a second implementation of `Order`, living
    /// in `cmd/blocked.rs`, that could only approximate `rank` (it has no
    /// topological ranks of its own) and so special-cased it. The daemon has the
    /// graph *and* the index, so it can answer in any order the shared
    /// comparator defines.
    Blocked {
        #[serde(default)]
        order: Order,
    },
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
    /// The filter set, carried whole rather than field by field.
    ///
    /// This was five scalars (`status`/`item_type`/`priority`/`assignee`/
    /// `label`) that the client unpacked and the daemon repacked — a hand-written
    /// translation on each side, and therefore two places a newly-added filter
    /// could be forgotten while every test still passed. Sending
    /// [`clove_core::view::Filters`] itself removes both.
    #[serde(default)]
    pub filters: clove_core::view::Filters,
    /// The result ordering (`--sort`/`--desc`). Defaults to `rank` ascending,
    /// which is what every client sent before the field existed.
    #[serde(default)]
    pub order: Order,
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
                order: Order::default(),
            },
            GraphRequest::Blocked {
                order: Order {
                    field: clove_core::view::SortField::Updated,
                    descending: true,
                },
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
                filters: clove_core::view::Filters::parse_multi(
                    &["open".to_owned(), "in_progress".to_owned()],
                    &["bug".to_owned()],
                    &["area:core".to_owned(), "area:ios".to_owned()],
                    Some("alice"),
                    &["1".to_owned(), "2".to_owned()],
                    Some("needle"),
                )
                .unwrap(),
                order: Order {
                    field: clove_core::view::SortField::Updated,
                    descending: true,
                },
                offset: 0,
                limit: Some(100),
            },
            QueryRequest {
                kind: QueryKind::Ready,
                filters: clove_core::view::Filters::default(),
                order: Order::default(),
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

    /// Why v5 is a *version bump* and not another compatible field.
    ///
    /// The codec is length-delimited JSON, so a v4 frame still **decodes** here
    /// — and that is the hazard, not the reassurance. A v4 client sends
    /// `"status": "open"` at the top level; this schema has no such field, so it
    /// is dropped and the daemon answers the *unfiltered* list. Nothing errors,
    /// nothing warns, and the client renders a result set that ignores its
    /// filter. The `ping` handshake in `client.rs` is what turns this into a
    /// clean mismatch, so the constant must stay ahead of the shape.
    #[test]
    fn a_v4_frame_decodes_but_loses_its_filters_which_is_why_the_version_moved() {
        const {
            assert!(
                PROTOCOL_VERSION >= 5,
                "the filter shape changed; the handshake must reject a v4 peer"
            )
        };
        let v4_frame = r#"{"kind":"list","status":"open","label":"area:core","offset":0}"#;
        let decoded: QueryRequest = serde_json::from_str(v4_frame).unwrap();
        assert_eq!(
            decoded.filters,
            clove_core::view::Filters::default(),
            "a v4 filter decodes as *no filter* — silently, which is the point"
        );
        assert_eq!(decoded.order, Order::default(), "absent order → rank asc");

        // And the reverse: a v5 frame carries the filters somewhere a v4 daemon
        // would never look.
        let v5_frame = serde_json::to_string(&QueryRequest {
            kind: QueryKind::List,
            filters: clove_core::view::Filters::parse(Some("open"), None, None, None, None)
                .unwrap(),
            order: Order::default(),
            offset: 0,
            limit: None,
        })
        .unwrap();
        assert!(v5_frame.contains("\"filters\""), "{v5_frame}");
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct V4QueryRequest {
            kind: QueryKind,
            #[serde(default)]
            status: Option<String>,
            offset: usize,
        }
        let old: V4QueryRequest = serde_json::from_str(&v5_frame).unwrap();
        assert!(
            old.status.is_none(),
            "a v4 daemon reads no status from a v5 frame"
        );
    }

    /// The service *shape* changed in v6 — the `search` RPC and `SearchRequest`
    /// were removed — so the constant had to move with it.
    ///
    /// Unlike the v5 filter change, neither direction of a mismatched pair is
    /// unsafe here: a v5 client calling a v6 daemon's absent `search` gets an
    /// error and falls back to its local path, and a v6 client never asks. The
    /// bump is policy (DESIGN §8.4: the constant gates a mixed-version pair
    /// whenever the shape changes) and it is what keeps the handshake honest —
    /// without it, `clove daemon status` would report a peer as compatible when
    /// its method set is not the one this crate declares.
    ///
    /// This assertion is a tripwire, not a proof: nothing can detect an
    /// added or removed tarpc method at runtime. It exists so that
    /// re-introducing a daemon-side search — which would put the §6.1
    /// file-vs-daemon divergence back — cannot be done without landing on this
    /// comment.
    #[test]
    fn removing_the_search_rpc_moved_the_protocol_version() {
        const {
            assert!(
                PROTOCOL_VERSION >= 6,
                "the service method set changed (search removed); bump the version"
            )
        };
    }
}
