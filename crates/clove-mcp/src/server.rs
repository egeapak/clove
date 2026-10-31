//! The rmcp MCP server: 15 tools spanning the agent read/write loop, each
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

use clove_service::{Engine, ToolError};
use clove_types::tool::*;

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

    /// Every tool name this server exposes, sourced from the generated tool
    /// router (so it can never drift from the `#[tool]` set). `clove setup` pins
    /// its Claude Code permission list to this.
    pub fn tool_names() -> Vec<String> {
        Self::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect()
    }

    /// Run a blocking engine call and map it to a tool result: `Ok` → structured
    /// JSON content; an engine error → an `isError` tool result carrying its
    /// text (not a protocol error); a task panic → a protocol-level internal
    /// error.
    async fn run<F>(&self, f: F) -> Result<CallToolResult, ErrorData>
    where
        F: FnOnce() -> Result<Value, ToolError> + Send + 'static,
    {
        match tokio::task::spawn_blocking(f).await {
            Ok(Ok(value)) => Ok(CallToolResult::structured(value)),
            Ok(Err(error)) => Ok(CallToolResult::error(vec![Content::text(
                error.to_string(),
            )])),
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
                       primary 'what should I work on?' query.",
        output_schema = crate::schema::item_page()
    )]
    async fn clove_ready(
        &self,
        Parameters(a): Parameters<FilterArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.ready(a)).await
    }

    #[tool(
        description = "List work items blocked by open OR missing dependencies, \
                       each with its `blocked_by` ids, ordered by (priority, \
                       topology). An item whose dependency id has no backing \
                       file is blocked too — that is a broken reference worth \
                       seeing, not a reason to omit it.",
        output_schema = crate::schema::item_page()
    )]
    async fn clove_blocked(
        &self,
        Parameters(a): Parameters<BlockedArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.blocked(a)).await
    }

    #[tool(
        description = "List work items with optional filters, ordered by \
                       (priority, topology, id).",
        output_schema = crate::schema::item_page()
    )]
    async fn clove_list(
        &self,
        Parameters(a): Parameters<ListArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.list(a)).await
    }

    #[tool(
        description = "Show one work item: its fields, the Markdown body, comment \
                       count, and computed `ready`/`blocked_by`."
    )]
    async fn clove_show(
        &self,
        Parameters(a): Parameters<IdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.show(a)).await
    }

    #[tool(
        description = "Search item titles, labels, and bodies for a \
                       case-insensitive SUBSTRING (not whole words: `core` \
                       matches `corepart`). Title matches rank first, then \
                       labels, then bodies.",
        output_schema = crate::schema::item_page()
    )]
    async fn clove_search(
        &self,
        Parameters(a): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.search(a)).await
    }

    #[tool(
        description = "Read an item's comment thread, oldest first. The read \
                       counterpart to clove_comment — clove_show reports only a \
                       `comment_count`. `limit` keeps the most recent comments; \
                       `offset` pages back through older ones.",
        output_schema = crate::schema::comment_page()
    )]
    async fn clove_comments(
        &self,
        Parameters(a): Parameters<CommentsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let e = self.engine.clone();
        self.run(move || e.comments(a)).await
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

    #[tool(
        description = "Create a new work item: one item per separate task, bug, \
                       or fix (the steps of one task are comments, not items). \
                       Give a `body` that stands on its own without this \
                       conversation: what is wrong or missing, where, and how to \
                       verify it is done. Returns its generated id, path, and title."
    )]
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

    #[tool(
        description = "Append a comment to an item. Returns the item id, the comment path, and the item title."
    )]
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
             of truth for this repository's tasks, bugs, and features. Track \
             work in clove for every request that has more than one task, or \
             one task with more than one step; file the items before you start, \
             even when clove has no items yet (a one-line edit needs no item). \
             First call clove_ready and clove_search / clove_list: if an open \
             item already describes a task, use it instead of filing a \
             duplicate. File one item per separate task, bug, or fix with \
             clove_new; the steps of one task are clove_comment entries on that \
             item, not separate items; a problem you find outside the current \
             task is a new item. Every item needs a self-contained `body` (what \
             is wrong or missing, where, and how to verify it is done), because \
             the reader does not see this conversation: leave out who asked, \
             deadlines, what was said earlier, how you found the problem, and \
             whether it was in scope, in titles, bodies, and comments alike. \
             When one task must wait for another, wire it with clove_dep_add. \
             Call clove_status in_progress when you begin an item; before \
             clove_status closed, add a clove_comment that says what changed and \
             how you verified it. When you mention an item to the user (in a \
             reply, a question, or a summary), write its id and then its title \
             in parentheses, for example `proj-7af3q2k9` (Fix the parser); never \
             a bare id. Explore with clove_show (detail), \
             clove_comments (the discussion thread — clove_show gives only a \
             count), clove_blocked, and clove_dep_tree; clove_stats for an \
             overview; clove_dep_remove / clove_set_parent to edit the graph. \
             Ids look like \
             `proj-7af3q2k9`. Read results are compacted: keys that are null or \
             an empty list are omitted, so a missing `assignee`, `labels`, or \
             `blocked_by` means unset or empty, never unknown. When scanning \
             many items pass `fields` to project to just what you need (e.g. \
             {\"fields\": [\"id\", \"title\", \"status\"]}), which is far \
             smaller; pass `compact: false` if you need every key present. Two live resources — clove://ready and \
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
            Ok(Err(error)) => Err(ErrorData::internal_error(error.to_string(), None)),
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
