//! The clove daemon IPC wire protocol (DESIGN §8.4).
//!
//! One request frame, one response frame, both JSON (see [`crate::frame`] for the
//! length-prefix framing). The types here are the single source of truth shared by
//! the daemon (`cloved`, server) and the CLI ([`crate::client`], client), so the
//! wire format can never drift between the two.
//!
//! v1 command set (DESIGN §8.4):
//! ```text
//! PING    → PONG
//! QUERY   { kind, filter, format, fields, limit, offset } → { envelope }
//! REINDEX → REINDEX_DONE { items_indexed, duration_ms, warnings }
//! STATUS  → { uptime_s, items_indexed, watcher_state, last_event_ms }
//! ```

use clove_core::{ItemStatus, ItemType, Priority};
use serde::{Deserialize, Serialize};

/// Wire-protocol version. Bumped on any incompatible change to the types below.
/// The CLI and daemon exchange it implicitly: a deserialization failure on either
/// side is treated as a version/garbage mismatch and the connection is dropped.
pub const PROTOCOL_VERSION: u32 = 1;

/// A request from the CLI to the daemon.
///
/// `#[serde(tag = "cmd")]` keeps the JSON self-describing and forward-compatible:
/// `{"cmd":"PING"}`, `{"cmd":"QUERY", ...}`. Unknown tags fail to deserialize,
/// which the server handles by returning [`Response::Error`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Request {
    /// Liveness probe. The CLI sends this first on every connection (§8.3).
    Ping,
    /// A read query the daemon answers from its hot index.
    Query(QueryRequest),
    /// Force a full reindex inside the daemon.
    Reindex,
    /// Operational daemon telemetry.
    Status,
}

/// Which lean list a [`Request::Query`] runs — mirrors `clove_index::QueryMode`.
/// Both `clove ls` and `clove query` are [`QueryKind::List`]; `clove ready` is
/// [`QueryKind::Ready`]. (`search`/`blocked` are not daemon-routed in M3 — they
/// fall back to the local path.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    /// Status/type/priority/label/assignee filter over all items.
    List,
    /// Unblocked open/in_progress items.
    Ready,
}

/// The payload of a [`Request::Query`]: the filter the daemon turns into a
/// `clove_index::Filter`. Carries the typed model values so the daemon and CLI
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

/// `QUERY` reply: the (page-limited) lean rows, the full unpaginated match count,
/// and any warnings. The CLI shapes these with its own list renderer, so
/// daemon-routed output is byte-identical to the local index path bar
/// `_meta.source = "daemon"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryListResponse {
    pub rows: Vec<LeanRow>,
    pub total: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// A response from the daemon to the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "resp", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Response {
    /// Reply to [`Request::Ping`].
    Pong,
    /// Reply to [`Request::Query`]: the lean rows + total the CLI shapes itself.
    QueryList(QueryListResponse),
    /// Reply to [`Request::Reindex`].
    ReindexDone(ReindexDone),
    /// Reply to [`Request::Status`].
    Status(StatusResponse),
    /// Any server-side failure (bad request, query error). The connection is then
    /// closed; the daemon stays up.
    Error(ErrorResponse),
}

/// `REINDEX_DONE` payload (DESIGN §8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReindexDone {
    pub items_indexed: u64,
    pub duration_ms: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// `STATUS` payload: the daemon's operational telemetry (DESIGN §8.4). This is the
/// daemon's *own* runtime state, not work-item analytics (that is the deferred M4
/// `clove stats`, see M3_PLAN §1.1).
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
}

/// A structured error reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Stable machine code, e.g. `"bad_request"`, `"query_failed"`.
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

impl ErrorResponse {
    /// Build an error response from a code and message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> ErrorResponse {
        ErrorResponse {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every request variant round-trips through JSON unchanged.
    #[test]
    fn request_round_trips() {
        let cases = vec![
            Request::Ping,
            Request::Reindex,
            Request::Status,
            Request::Query(QueryRequest {
                kind: QueryKind::List,
                status: Some(ItemStatus::Open),
                item_type: Some(ItemType::Bug),
                priority: None,
                assignee: Some("alice".to_owned()),
                label: Some("area:core".to_owned()),
                offset: 0,
                limit: Some(100),
            }),
            Request::Query(QueryRequest {
                kind: QueryKind::Ready,
                status: None,
                item_type: None,
                priority: None,
                assignee: None,
                label: None,
                offset: 20,
                limit: None,
            }),
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            let back: Request = serde_json::from_str(&json).unwrap();
            assert_eq!(case, back, "round-trip mismatch for {json}");
        }
    }

    /// Every response variant round-trips through JSON unchanged.
    #[test]
    fn response_round_trips() {
        let cases = vec![
            Response::Pong,
            Response::QueryList(QueryListResponse {
                rows: vec![LeanRow {
                    id: "proj-7af".to_owned(),
                    status: "open".to_owned(),
                    item_type: "feature".to_owned(),
                    priority: 1,
                    title: "do the thing".to_owned(),
                }],
                total: 1,
                warnings: vec![],
            }),
            Response::ReindexDone(ReindexDone {
                items_indexed: 42,
                duration_ms: 735,
                warnings: vec!["dangling dep".to_owned()],
            }),
            Response::Status(StatusResponse {
                uptime_s: 10,
                items_indexed: 7,
                watcher_state: "watching".to_owned(),
                last_event_ms: Some(1200),
                batches_applied: 3,
            }),
            Response::Error(ErrorResponse::new("bad_request", "unknown cmd")),
        ];
        for case in cases {
            let json = serde_json::to_string(&case).unwrap();
            let back: Response = serde_json::from_str(&json).unwrap();
            assert_eq!(case, back, "round-trip mismatch for {json}");
        }
    }

    /// The tag wire form is the documented uppercase command name.
    #[test]
    fn ping_tag_is_uppercase() {
        let json = serde_json::to_string(&Request::Ping).unwrap();
        assert_eq!(json, r#"{"cmd":"PING"}"#);
        let json = serde_json::to_string(&Response::Pong).unwrap();
        assert_eq!(json, r#"{"resp":"PONG"}"#);
    }
}
