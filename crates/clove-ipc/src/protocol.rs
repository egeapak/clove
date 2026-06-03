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

/// Which read a [`Request::Query`] runs. Mirrors the CLI's read commands so the
/// daemon can dispatch to the matching `clove-index` query path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    /// `clove ls` — the lean list projection.
    Ls,
    /// `clove ready` — unblocked open/in_progress items.
    Ready,
    /// `clove blocked` — items with unresolved deps.
    Blocked,
    /// `clove query` — structured filter.
    Query,
    /// `clove search` — full-text search.
    Search,
}

/// The payload of a [`Request::Query`] (DESIGN §8.4 `QUERY { filter, format,
/// fields }`, extended with the pagination + kind the daemon needs to dispatch).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRequest {
    /// Which read command this is.
    pub kind: QueryKind,
    /// Opaque structured filter (the CLI's JSON filter object), passed through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<serde_json::Value>,
    /// Free-text argument for [`QueryKind::Search`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Output format string (`json`/`jsonl`/`human`), echoed in the envelope.
    pub format: String,
    /// Optional `--fields` projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<String>>,
    /// Result cap (`--limit`); `None` = command default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Result offset (`--offset`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

/// A response from the daemon to the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "resp", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Response {
    /// Reply to [`Request::Ping`].
    Pong,
    /// Reply to [`Request::Query`]: the full standard CLI envelope
    /// (`{v, ok, data, _meta}`) the daemon built, carried verbatim so the CLI can
    /// print it unchanged. Kept as a raw value to avoid coupling this lean crate
    /// to the CLI's item JSON shapes.
    Query { envelope: serde_json::Value },
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
                kind: QueryKind::Search,
                filter: None,
                text: Some("hello".to_owned()),
                format: "json".to_owned(),
                fields: Some(vec!["id".to_owned(), "title".to_owned()]),
                limit: Some(100),
                offset: None,
            }),
            Request::Query(QueryRequest {
                kind: QueryKind::Ready,
                filter: Some(serde_json::json!({"status": "open"})),
                text: None,
                format: "human".to_owned(),
                fields: None,
                limit: None,
                offset: Some(20),
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
            Response::Query {
                envelope: serde_json::json!({"v": 1, "ok": true, "data": []}),
            },
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
