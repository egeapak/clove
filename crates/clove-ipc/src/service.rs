//! The typed CLI<->daemon RPC service (tarpc).
//!
//! `#[tarpc::service]` generates [`CloveRpcClient`] (used by the blocking
//! [`crate::client::DaemonClient`] wrapper and the MCP shim) and the `CloveRpc`
//! server trait (implemented by `cloved`). The request/response *payload* types
//! still live in [`crate::protocol`]; this module defines the service contract
//! and a serializable error, replacing the old hand-rolled `Request`/`Response`
//! enums + frame codec.

use clove_types::{EditRequest, ItemStatus, NewSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::protocol::{
    GraphRequest, GraphResponse, QueryListResponse, QueryRequest, ReindexDone, SearchRequest,
    StatusResponse,
};

/// A serializable RPC error returned by fallible daemon methods (mirrors the old
/// `ErrorResponse`): a stable machine `code` plus a human `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{code}: {message}")]
pub struct RpcError {
    /// Stable machine code, e.g. `"bad_request"`, `"query_failed"`.
    pub code: String,
    /// Human-readable detail.
    pub message: String,
}

impl RpcError {
    /// Build an RPC error from a code and message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> RpcError {
        RpcError {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// The clove daemon RPC service (DESIGN §8.4). Read/graph/reindex/status today;
/// the M4 mutation methods are added in the next phase.
#[tarpc::service]
pub trait CloveRpc {
    /// Liveness probe: returns the daemon's [`crate::PROTOCOL_VERSION`].
    async fn ping() -> u32;
    /// Operational telemetry (uptime, items indexed, watcher state, …).
    async fn status() -> StatusResponse;
    /// A lean list query (`ls`/`ready`/`query`): page-limited rows + total count.
    async fn query(req: QueryRequest) -> Result<QueryListResponse, RpcError>;
    /// Full-text search; matched item ids in FTS-rank order.
    async fn search(req: SearchRequest) -> Result<Vec<String>, RpcError>;
    /// A dependency-graph query served from the daemon's cached graph.
    async fn graph(req: GraphRequest) -> Result<GraphResponse, RpcError>;
    /// Force a full reindex inside the daemon; returns its report.
    async fn reindex() -> Result<ReindexDone, RpcError>;

    // ---- M4 mutations + reads (topology B: writes serialized through the
    // single daemon, which keeps its index/graph coherent). Each returns the
    // §7.4 item JSON (or `{id, path}`) so every surface shares one shape.

    /// Create an item; returns `{ id, path }`.
    async fn create(spec: NewSpec) -> Result<Value, RpcError>;
    /// Transition an item's status; returns the updated item object.
    async fn set_status(id: String, status: ItemStatus) -> Result<Value, RpcError>;
    /// Apply `KEY=VALUE` edits atomically; returns the updated item object.
    /// Retained for back-compat; new clients prefer [`CloveRpc::apply_edit`].
    async fn edit(id: String, assignments: Vec<String>) -> Result<Value, RpcError>;
    /// Apply a structured [`EditRequest`] atomically (supports body edits, label
    /// set/delta, assignee clear); returns the updated item object.
    async fn apply_edit(id: String, req: EditRequest) -> Result<Value, RpcError>;
    /// Append a comment; returns `{ id, path }`.
    async fn add_comment(id: String, author: String, body: String) -> Result<Value, RpcError>;
    /// Add a hard dependency `id → dep_id`; returns the updated item object.
    async fn dep_add(id: String, dep_id: String) -> Result<Value, RpcError>;
    /// Remove a hard dependency `id → dep_id`; returns the updated item object.
    async fn dep_remove(id: String, dep_id: String) -> Result<Value, RpcError>;
    /// Set (or, with `parent = None`, clear) an item's parent; returns the
    /// updated item object.
    async fn set_parent(id: String, parent: Option<String>) -> Result<Value, RpcError>;
    /// Full item detail (frontmatter + body + comment_count + ready/blocked_by).
    async fn show(id: String) -> Result<Value, RpcError>;
    /// Work-item analytics (`clove stats`) as JSON.
    async fn stats(top: u32, include_epics: bool) -> Result<Value, RpcError>;
}
