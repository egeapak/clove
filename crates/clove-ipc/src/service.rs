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

use crate::hub::{Detached, HubStatus};
use crate::protocol::{
    GraphRequest, GraphResponse, QueryListResponse, QueryRequest, ReindexDone, StatusResponse,
};

/// The exit code carried by an [`RpcError`] that predates the `exit` field, and
/// by daemon-internal failures that do not map to a [`clove_types::CloveError`]:
/// 7, "daemon error" (DESIGN §7.6).
fn default_rpc_exit() -> u8 {
    7
}

/// A serializable RPC error returned by fallible daemon methods (mirrors the old
/// `ErrorResponse`): a stable machine `code`, a human `message`, and the numeric
/// `exit` the failure classifies to.
///
/// `exit` lets the client reproduce the caller's exit code without re-deriving a
/// taxonomy the daemon already computed — a `NotFound` rejected by the daemon
/// must exit 2 exactly as it would have locally. The wire is length-delimited
/// JSON (self-describing), so the defaulted field is compatible in both
/// directions with a daemon or client that predates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{code}: {message}")]
pub struct RpcError {
    /// Stable machine code, e.g. `"ITEM_NOT_FOUND"`, `"query_failed"`.
    pub code: String,
    /// Human-readable detail.
    pub message: String,
    /// Numeric exit code for this failure (DESIGN §7.6).
    #[serde(default = "default_rpc_exit")]
    pub exit: u8,
}

impl RpcError {
    /// Build an RPC error from a code and message, classified as a daemon error
    /// (exit 7). For a failure that carries a `CloveError`'s own classification,
    /// use [`RpcError::with_exit`].
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> RpcError {
        RpcError {
            code: code.into(),
            message: message.into(),
            exit: default_rpc_exit(),
        }
    }

    /// Build an RPC error carrying an explicit exit code.
    pub fn with_exit(code: impl Into<String>, message: impl Into<String>, exit: u8) -> RpcError {
        RpcError {
            code: code.into(),
            message: message.into(),
            exit,
        }
    }
}

/// The project a call is about: the caller's **own** `.clove/` directory,
/// absolute (and canonical, as far as the caller can resolve it), plus whether
/// the call may load the project if the daemon is not serving it yet.
///
/// Every project-scoped call carries one. It is the caller's identity, not a
/// target it may choose: a client only ever sends the project it discovered for
/// itself, and there is no parameter anywhere that names another project. The
/// daemon rejects a relative path (`BAD_PROJECT`) — relative to what would
/// depend on a working directory the daemon does not share with the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub clove_dir: String,
    /// Load the project if needed. Reads send `false`: asking whether a daemon
    /// can answer must not start serving a project.
    pub load: bool,
    /// The project's `.clove/daemon.token`. The daemon refuses a call whose
    /// token is not the one in `clove_dir` (`BAD_TOKEN`), so a client acts
    /// only on a project whose `.clove/` it can read. A gate for automation,
    /// not a security boundary against the local user.
    pub token: String,
}

/// The clove daemon RPC service (DESIGN §8.4). One daemon serves every project
/// of a user; each project-scoped call names its caller's [`Project`], and the
/// daemon resolves it per call.
#[tarpc::service]
pub trait CloveRpc {
    /// Liveness probe: returns the daemon's [`crate::PROTOCOL_VERSION`].
    async fn ping() -> u32;
    /// The daemon and every project it serves.
    async fn hub_status() -> HubStatus;
    /// Is this project served (loading it first when `project.load`)? The
    /// liveness probe and heartbeat of a project client; counts as a ping.
    async fn attach(project: Project) -> Result<(), RpcError>;
    /// Stop serving this project. Returns once its teardown has run; when it
    /// was the last one the daemon exits right after replying.
    async fn detach(project: Project) -> Result<Detached, RpcError>;
    /// Operational telemetry (uptime, items indexed, watcher state, …).
    async fn status(project: Project) -> Result<StatusResponse, RpcError>;
    /// A monotonic counter bumped on every graph-affecting change (watcher batch,
    /// daemon-side write, drift-triggered refresh, reindex). A cheap lock-free
    /// atomic load; the MCP server polls it to push `resources/updated` on change.
    async fn change_generation(project: Project) -> Result<u64, RpcError>;
    /// A lean list query (`ls`/`ready`/`query`): page-limited rows + total count.
    async fn query(project: Project, req: QueryRequest) -> Result<QueryListResponse, RpcError>;
    // There is deliberately **no `search`** here. v5 had one — the daemon ran the
    // index's FTS5 query and returned matched ids — and it is gone with the FTS
    // table (index schema 6, read-path roadmap §6.1): search is a parallel file
    // scan on every surface now, which the client performs itself. Re-adding a
    // daemon search would reintroduce the divergence the removal closed, because
    // the daemon's answer would have to come from something other than the file
    // scan the `--no-index` path runs.
    /// A dependency-graph query served from the daemon's cached graph.
    async fn graph(project: Project, req: GraphRequest) -> Result<GraphResponse, RpcError>;
    /// Force a full reindex inside the daemon; returns its report.
    async fn reindex(project: Project) -> Result<ReindexDone, RpcError>;

    // ---- M4 mutations + reads (topology B: writes serialized through the
    // single daemon, which keeps its index/graph coherent). Each returns the
    // §7.4 item JSON (or `{id, path}`) so every surface shares one shape.

    /// Create an item; returns `{ id, path }`.
    async fn create(project: Project, spec: NewSpec) -> Result<Value, RpcError>;
    /// Transition an item's status; returns the updated item object.
    async fn set_status(
        project: Project,
        id: String,
        status: ItemStatus,
    ) -> Result<Value, RpcError>;
    /// Apply `KEY=VALUE` edits atomically; returns the updated item object.
    /// Retained for back-compat; new clients prefer [`CloveRpc::apply_edit`].
    async fn edit(
        project: Project,
        id: String,
        assignments: Vec<String>,
    ) -> Result<Value, RpcError>;
    /// Apply a structured [`EditRequest`] atomically (supports body edits, label
    /// set/delta, assignee clear); returns the updated item object.
    async fn apply_edit(project: Project, id: String, req: EditRequest) -> Result<Value, RpcError>;
    /// Append a comment; returns `{ id, path }`.
    async fn add_comment(
        project: Project,
        id: String,
        author: String,
        body: String,
    ) -> Result<Value, RpcError>;
    /// Add a hard dependency `id → dep_id`; returns the updated item object.
    async fn dep_add(project: Project, id: String, dep_id: String) -> Result<Value, RpcError>;
    /// Remove a hard dependency `id → dep_id`; returns the updated item object.
    async fn dep_remove(project: Project, id: String, dep_id: String) -> Result<Value, RpcError>;
    /// Set (or, with `parent = None`, clear) an item's parent; returns the
    /// updated item object.
    async fn set_parent(
        project: Project,
        id: String,
        parent: Option<String>,
    ) -> Result<Value, RpcError>;
    /// Full item detail (frontmatter + body + comment_count + ready/blocked_by).
    async fn show(project: Project, id: String) -> Result<Value, RpcError>;
    /// Work-item analytics (`clove stats`) as JSON.
    async fn stats(project: Project, top: u32, include_epics: bool) -> Result<Value, RpcError>;
}
