//! The rmcp MCP server: 12 tools spanning the agent read/write loop, each
//! delegating to the [`Engine`]. Tool bodies run on a blocking task (the engine
//! does file I/O and, for writes, drives the blocking daemon client).

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use serde_json::Value;

use crate::args::*;
use crate::engine::Engine;

/// The MCP server handler. Cheap to clone (the engine is just paths + config).
#[derive(Clone)]
pub struct CloveServer {
    engine: Engine,
}

impl CloveServer {
    pub fn new(engine: Engine) -> Self {
        Self { engine }
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
                       type, title, assignee, and label add/remove."
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
}

#[tool_handler(
    name = "clove",
    instructions = "clove is a fast, dependency-aware work-item tracker. Use \
                    clove_ready to find unblocked work, clove_show for detail, \
                    clove_list/clove_blocked/clove_search/clove_dep_tree to \
                    explore, clove_stats for an overview, and clove_new / \
                    clove_status / clove_edit / clove_comment / clove_dep_add to \
                    record progress. Ids look like `proj-7af3q2k9`."
)]
impl ServerHandler for CloveServer {}
