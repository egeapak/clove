//! The rmcp MCP server: 14 tools spanning the agent read/write loop, each
//! delegating to the [`Engine`]. Tool bodies run on a blocking task (the engine
//! does file I/O and, for writes, drives the blocking daemon client).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolResult, Content, Implementation, ListResourcesResult,
    PaginatedRequestParams, RawResource, ReadResourceRequestParams, ReadResourceResult,
    ResourceContents, ServerCapabilities, ServerInfo, SubscribeRequestParams,
    UnsubscribeRequestParams,
};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_handler, tool_router, ErrorData, RoleServer, ServerHandler};
use serde_json::Value;

use crate::args::*;
use crate::engine::Engine;

/// The two live resources clove exposes. Their contents change on every
/// graph-affecting mutation; a subscribed client is pushed `resources/updated`
/// (see the notifier in `lib.rs`).
pub const READY_URI: &str = "clove://ready";
pub const STATS_URI: &str = "clove://stats";

/// The MCP server handler. Cheap to clone (the engine is just paths + config; the
/// subscription set is shared behind an `Arc`).
#[derive(Clone)]
pub struct CloveServer {
    engine: Engine,
    /// Resource URIs the connected client has subscribed to. The notifier only
    /// pushes `resources/updated` for URIs in this set (per the MCP spec).
    subscriptions: Arc<Mutex<HashSet<String>>>,
}

impl CloveServer {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// A handle to the shared subscription set, for the notifier loop in `lib.rs`.
    pub fn subscriptions(&self) -> Arc<Mutex<HashSet<String>>> {
        self.subscriptions.clone()
    }

    /// Run a blocking engine call and map it to a tool result: `Ok` → structured
    /// JSON content; an engine error string → an `isError` tool result (not a
    /// protocol error); a task panic → a protocol-level internal error.
    async fn run<F>(&self, f: F) -> Result<CallToolResult, ErrorData>
    where
        F: FnOnce() -> Result<Value, String> + Send + 'static,
    {
        match tokio::task::spawn_blocking(f).await {
            Ok(Ok(value)) => Ok(CallToolResult::structured(value)),
            Ok(Err(message)) => Ok(CallToolResult::error(vec![Content::text(message)])),
            Err(join) => Err(ErrorData::internal_error(
                format!("tool task failed: {join}"),
                None,
            )),
        }
    }
}

#[tool_router]
impl CloveServer {
    #[tool(
        description = "List work items ready to start now: open/in-progress items \
                       whose hard dependencies are all closed and which have no \
                       dangling dependencies, ordered by (priority, topology). The \
                       primary 'what should I work on?' query."
    )]
    async fn clove_ready(
        &self,
        Parameters(a): Parameters<FilterArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.ready(a)).await
    }

    #[tool(
        description = "List work items blocked by open or missing dependencies, \
                       each with its `blocked_by` ids, ordered by (priority, topology)."
    )]
    async fn clove_blocked(
        &self,
        Parameters(a): Parameters<BlockedArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.blocked(a)).await
    }

    #[tool(description = "List work items with optional filters, ordered by \
                          (priority, topology, id).")]
    async fn clove_list(
        &self,
        Parameters(a): Parameters<ListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.list(a)).await
    }

    #[tool(
        description = "Show one work item in full: all fields, the Markdown body, \
                       comment count, and computed `ready`/`blocked_by`."
    )]
    async fn clove_show(
        &self,
        Parameters(a): Parameters<IdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.show(a)).await
    }

    #[tool(description = "Full-text search over item titles, labels, and bodies \
                          (case-insensitive; title matches rank first).")]
    async fn clove_search(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.search(a)).await
    }

    #[tool(description = "Render the dependency tree rooted at an item, with \
                          per-node status, `ready`, and `cycle_ref` markers.")]
    async fn clove_dep_tree(
        &self,
        Parameters(a): Parameters<DepTreeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.dep_tree(a)).await
    }

    #[tool(description = "Repository analytics: counts by status/type/priority/\
                       assignee/label, ready/blocked/excluded/dangling totals, \
                       cycle count, per-epic rollups, and throughput.")]
    async fn clove_stats(
        &self,
        Parameters(a): Parameters<StatsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.stats(a)).await
    }

    #[tool(description = "Create a new work item. Returns its generated id and path.")]
    async fn clove_new(
        &self,
        Parameters(a): Parameters<NewArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.create(a)).await
    }

    #[tool(
        description = "Change an item's status (open | in_progress | closed). \
                       Closing sets the closed timestamp; reopening clears it."
    )]
    async fn clove_status(
        &self,
        Parameters(a): Parameters<StatusArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.set_status(a)).await
    }

    #[tool(
        description = "Edit an item's fields in one atomic write: status, priority, \
                       type, title, assignee, Markdown body, and label add/remove."
    )]
    async fn clove_edit(
        &self,
        Parameters(a): Parameters<EditArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.edit(a)).await
    }

    #[tool(description = "Append a comment to an item.")]
    async fn clove_comment(
        &self,
        Parameters(a): Parameters<CommentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.comment(a)).await
    }

    #[tool(
        description = "Add a hard dependency: `id` depends on `dep_id`. Rejects \
                       self-loops and dependencies that would create a cycle."
    )]
    async fn clove_dep_add(
        &self,
        Parameters(a): Parameters<DepAddArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.dep_add(a)).await
    }

    #[tool(
        description = "Remove a hard dependency: `id` no longer depends on `dep_id`. \
                       Errors if no such dependency exists."
    )]
    async fn clove_dep_remove(
        &self,
        Parameters(a): Parameters<DepAddArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.dep_remove(a)).await
    }

    #[tool(
        description = "Set or clear an item's parent (epic membership). Omit `parent` \
                       to clear it. Rejects self-parenting and parent cycles."
    )]
    async fn clove_set_parent(
        &self,
        Parameters(a): Parameters<SetParentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.set_parent(a)).await
    }
}

// `#[tool_handler]` generates `call_tool`/`list_tools`; it skips `get_info`
// because we provide our own (to advertise the resources capability), so the
// server info + instructions move into `get_info` below.
#[tool_handler]
impl ServerHandler for CloveServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            // Order matters (builder type-state): resources sub-toggles only exist
            // after `enable_resources()`.
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .enable_resources_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new("clove", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "clove is a fast, dependency-aware work-item tracker and the source \
             of truth for this repository's tasks, bugs, and features. Prefer it \
             over ad-hoc TODO lists for any multi-step work: at the start of a \
             task call clove_ready (unblocked work) and clove_search / clove_list \
             to find existing items before creating new ones, then record \
             progress as you go — clove_new to file work, clove_status to \
             transition it (open/in_progress/closed), and clove_comment to note \
             findings. Explore with clove_show (detail), clove_blocked, and \
             clove_dep_tree; clove_stats for an overview; clove_dep_add / \
             clove_dep_remove / clove_set_parent to wire the graph. Ids look like \
             `proj-7af3q2k9`. Two live resources — clove://ready and \
             clove://stats — mirror the ready queue and the repo overview; \
             subscribe to be pushed resources/updated whenever the work graph \
             changes."
                .to_owned(),
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            RawResource::new(READY_URI, "ready-queue")
                .with_description(
                    "Work items ready to start now (open/in-progress, all hard \
                     dependencies closed, no dangling deps). Same JSON as clove_ready.",
                )
                .with_mime_type("application/json")
                .no_annotation(),
            RawResource::new(STATS_URI, "overview")
                .with_description(
                    "Repository analytics: counts by status/type/priority, \
                     ready/blocked, epics, throughput. Same JSON as clove_stats.",
                )
                .with_mime_type("application/json")
                .no_annotation(),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let engine = self.engine.clone();
        let uri = request.uri.clone();
        // Reads do file I/O (same as the tools) → run on a blocking task.
        let result = match uri.as_str() {
            READY_URI => {
                tokio::task::spawn_blocking(move || engine.ready(FilterArgs::default())).await
            }
            STATS_URI => {
                tokio::task::spawn_blocking(move || engine.stats(StatsArgs::default())).await
            }
            other => {
                return Err(ErrorData::resource_not_found(
                    format!("unknown resource: {other}"),
                    None,
                ));
            }
        };
        match result {
            Ok(Ok(value)) => {
                let json = serde_json::to_string(&value).map_err(|e| {
                    ErrorData::internal_error(format!("serialize resource: {e}"), None)
                })?;
                Ok(ReadResourceResult::new(vec![ResourceContents::text(
                    json, uri,
                )
                .with_mime_type("application/json")]))
            }
            // Unlike a tool call (which wraps a repo error in `isError`), a resource
            // read surfaces the error at the protocol level.
            Ok(Err(message)) => Err(ErrorData::internal_error(message, None)),
            Err(join) => Err(ErrorData::internal_error(
                format!("resource task failed: {join}"),
                None,
            )),
        }
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        if let Ok(mut subs) = self.subscriptions.lock() {
            subs.insert(request.uri);
        }
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        if let Ok(mut subs) = self.subscriptions.lock() {
            subs.remove(&request.uri);
        }
        Ok(())
    }
}
