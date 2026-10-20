//! The tool engine: one sync method per tool of the clove tool surface,
//! decoupled from any transport. `clove-mcp` serves it over MCP stdio and the
//! `clove` host serves it to plugins over the host IPC channel.
//!
//! Topology B: **writes** prefer the single `cloved` daemon (which serializes
//! them and keeps its index/graph coherent) and fall back to direct `clove-core`
//! ops when no daemon is reachable. **Reads** now go through
//! [`clove_engine::Engine`], which tiers them the same way — daemon, then the
//! local SQLite index, then the files — so a hot daemon serves the MCP tools
//! instead of sitting idle beside a full store scan per call (read-path roadmap
//! §4). The tools keep their full-item output: a tier answers the *query* (the
//! filtering, ordering, and counting happen in SQL) and only the returned page
//! is read from disk.
//!
//! Every method returns the §7.4 JSON shape or a [`ToolError`], now with a
//! `source` key naming the tier that answered — the `_meta.source` of the CLI
//! and web, carried as a plain key because the MCP page has no `_meta`. Methods
//! are blocking: run them on a blocking task or a plain thread, never inside a
//! tokio runtime (the daemon client calls `block_on`).

use camino::Utf8PathBuf;
use chrono::Utc;
use clove_core::ops;
use clove_core::{Filters, ItemStore};
use clove_engine::{ListAnswer, Projection, Rows};
use clove_ipc::{ClientError, DaemonClient};
use clove_types::{CloveId, ItemStatus, ItemType, NewSpec};
use serde_json::Value;

use clove_types::tool::*;

use crate::daemon::Daemon;
use crate::error::ToolError;
use crate::shape::{self, Shape};
use clove_core::view::defaults::{DEP_TREE_DEPTH, STATS_TOP};
use clove_core::view::{Order, Page, SearchOrder};

/// Shared, cheap-to-clone context for the tools.
#[derive(Clone)]
pub struct Engine {
    /// The `.clove/` directory (for daemon probing).
    pub clove_dir: Utf8PathBuf,
    /// The repository root (for the file store).
    pub repo_root: Utf8PathBuf,
    /// Id prefix + default type for `create` (from `.clove/config`).
    pub id_prefix: String,
    pub default_type: ItemType,
    /// The daemon write coordinator (see [`Daemon`]).
    daemon: Daemon,
    /// The read tier. Holds its own cached daemon connection for the same
    /// reason: this server is long-lived and re-probing per tool call would
    /// cost more than the read it accelerates.
    reads: clove_engine::Engine,
}

impl Engine {
    pub fn new(
        clove_dir: Utf8PathBuf,
        repo_root: Utf8PathBuf,
        id_prefix: String,
        default_type: ItemType,
        tiers: clove_engine::Tiers,
    ) -> Self {
        // The caller picks the read tiers: the MCP server passes
        // `Tiers::for_repo` (every tier, fast staleness check, the repo's
        // `index.auto_refresh` policy); the plugin host derives them from its
        // own `--no-index` / `--deep` flags so a plugin's reads honour them.
        let reads =
            clove_engine::Engine::new(ItemStore::new(repo_root.clone()), clove_dir.clone(), tiers);
        let daemon = Daemon::new(clove_dir.clone());
        Self {
            clove_dir,
            repo_root,
            id_prefix,
            default_type,
            daemon,
            reads,
        }
    }

    fn store(&self) -> ItemStore {
        ItemStore::new(self.repo_root.clone())
    }

    /// Run `call` against the daemon write coordinator, if one is reachable;
    /// see [`Daemon::with`].
    fn with_daemon<T>(
        &self,
        call: impl FnOnce(&mut DaemonClient) -> Result<T, ClientError>,
    ) -> Option<Result<T, ToolError>> {
        self.daemon.with(call)
    }

    // ---- Read tools (tiered through clove-engine) ---------------------------

    pub fn ready(&self, a: FilterArgs) -> Result<Value, ToolError> {
        let filters = filters_of(&a)?;
        let order = order_of(&a)?;
        let window = window(a.offset, a.limit);
        let answer = self
            .reads
            .ready(&filters, order, window, Projection::Full)
            .map_err(ToolError::Core)?;
        Ok(shape::apply(
            page_of(
                &answer,
                window,
                order.field.as_str(),
                order.dir_str(),
                Some(&filters),
            ),
            &shaping(&a.shape),
        ))
    }

    pub fn blocked(&self, a: BlockedArgs) -> Result<Value, ToolError> {
        let filters = filters_of(&a.filter)?;
        let order = order_of(&a.filter)?;
        let window = window(a.filter.offset, a.filter.limit);
        let answer = self
            .reads
            .blocked(&filters, order, window, Projection::Full)
            .map_err(ToolError::Core)?;
        Ok(shape::apply(
            page_of(
                &answer,
                window,
                order.field.as_str(),
                order.dir_str(),
                Some(&filters),
            ),
            &shaping(&a.filter.shape),
        ))
    }

    pub fn list(&self, a: ListArgs) -> Result<Value, ToolError> {
        let filters = filters_of(&a.filter)?;
        let order = order_of(&a.filter)?;
        let window = window(a.filter.offset, a.filter.limit);
        let answer = self
            .reads
            .list(&filters, order, window, Projection::Full)
            .map_err(ToolError::Core)?;
        Ok(shape::apply(
            page_of(
                &answer,
                window,
                order.field.as_str(),
                order.dir_str(),
                Some(&filters),
            ),
            &shaping(&a.filter.shape),
        ))
    }

    pub fn show(&self, a: IdArgs) -> Result<Value, ToolError> {
        let id = parse_id(&a.id)?;
        let shaping = shaping(&a.shape);
        self.reads
            .show(&id)
            .map(|answer| shape::apply(answer.value, &shaping))
            .map_err(ToolError::Core)
    }

    pub fn search(&self, a: SearchArgs) -> Result<Value, ToolError> {
        let order =
            SearchOrder::parse(a.sort.as_deref(), dir_of(a.desc)).map_err(ToolError::Core)?;
        let window = window(a.offset, a.limit);
        let answer = self
            .reads
            .search(&a.text, order, window)
            .map_err(ToolError::Core)?;
        Ok(shape::apply(
            // No `filters` echo: `search` takes no field filters, and an empty
            // object would advertise a surface the tool does not have.
            page_of(
                &answer,
                window,
                order.reported_sort(),
                order.dir_str(),
                None,
            ),
            &shaping(&a.shape),
        ))
    }

    pub fn comments(&self, a: CommentsArgs) -> Result<Value, ToolError> {
        let id = parse_id(&a.id)?;
        self.reads
            .comments(&id, window(a.skip_newest, a.limit))
            .map(|answer| answer.value)
            .map_err(ToolError::Core)
    }

    pub fn dep_tree(&self, a: DepTreeArgs) -> Result<Value, ToolError> {
        let id = parse_id(&a.id)?;
        // Deliberately unshaped: `dep-tree.json` lists `children` as *required*
        // on every node, so compaction — which recurses and would drop the empty
        // `children` of every leaf — puts the payload outside its published v1
        // schema. The schema-legality argument for compacting items comes from
        // `item.json`'s much smaller `required` set and does not transfer here.
        // `0` is unlimited here as it is for every other bound (`--depth 0`,
        // `--limit 0`, `--top 0`). It used to pass straight through, so a client
        // asking for the whole tree got the root and no children.
        let depth = match a.depth.unwrap_or(DEP_TREE_DEPTH as u64) as usize {
            0 => usize::MAX,
            n => n,
        };
        self.reads
            .dep_tree(&id, depth)
            .map(|answer| answer.value)
            .map_err(ToolError::Core)
    }

    pub fn stats(&self, a: StatsArgs) -> Result<Value, ToolError> {
        self.reads
            .stats(
                a.top.unwrap_or(STATS_TOP as u64) as usize,
                !a.no_epics.unwrap_or(false),
                Utc::now(),
            )
            .map(|answer| answer.value)
            .map_err(ToolError::Core)
    }

    // ---- Write tools (daemon-preferred, ops fallback) -----------------------

    pub fn create(&self, a: NewArgs) -> Result<Value, ToolError> {
        let spec = NewSpec {
            title: a.title,
            item_type: a.item_type,
            priority: a.priority,
            labels: a.labels.unwrap_or_default(),
            deps: a.deps.unwrap_or_default(),
            parent: a.parent,
            assignee: a.assignee,
            body: a.body,
        };
        match self.with_daemon(|d| d.create(spec.clone())) {
            Some(result) => result,
            None => ops::create(
                &self.store(),
                &self.id_prefix,
                self.default_type,
                spec,
                Utc::now(),
            )
            .map_err(ToolError::Core),
        }
    }

    pub fn set_status(&self, a: StatusArgs) -> Result<Value, ToolError> {
        let status = ItemStatus::parse(&a.status).map_err(ToolError::Core)?;
        match self.with_daemon(|d| d.set_status(a.id.clone(), status)) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                ops::transition(&self.store(), &id, status, Utc::now()).map_err(ToolError::Core)
            }
        }
    }

    pub fn edit(&self, a: EditArgs) -> Result<Value, ToolError> {
        let req = a.to_request().map_err(ToolError::Core)?;
        if req.is_empty() {
            return Err(ToolError::Usage("no fields to edit".to_owned()));
        }
        match self.with_daemon(|d| d.apply_edit(a.id.clone(), req.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                clove_core::apply_edit(&self.store(), &id, &req, Utc::now())
                    .map_err(ToolError::Core)
            }
        }
    }

    pub fn comment(&self, a: CommentArgs) -> Result<Value, ToolError> {
        let author = clove_core::comments::comment_author(&self.repo_root);
        match self.with_daemon(|d| d.add_comment(a.id.clone(), author.clone(), a.message.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                ops::comment(&self.store(), &id, &author, &a.message).map_err(ToolError::Core)
            }
        }
    }

    pub fn dep_add(&self, a: DepAddArgs) -> Result<Value, ToolError> {
        match self.with_daemon(|d| d.dep_add(a.id.clone(), a.dep_id.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                let dep = parse_id(&a.dep_id)?;
                ops::dep_add(&self.store(), &id, &dep, Utc::now()).map_err(ToolError::Core)
            }
        }
    }

    pub fn dep_remove(&self, a: DepAddArgs) -> Result<Value, ToolError> {
        match self.with_daemon(|d| d.dep_remove(a.id.clone(), a.dep_id.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                let dep = parse_id(&a.dep_id)?;
                ops::dep_remove(&self.store(), &id, &dep, Utc::now()).map_err(ToolError::Core)
            }
        }
    }

    pub fn set_parent(&self, a: SetParentArgs) -> Result<Value, ToolError> {
        match self.with_daemon(|d| d.set_parent(a.id.clone(), a.parent.clone())) {
            Some(result) => result,
            None => {
                let id = parse_id(&a.id)?;
                let parent = match a.parent {
                    Some(p) => Some(parse_id(&p)?),
                    None => None,
                };
                ops::set_parent(&self.store(), &id, parent.as_ref(), Utc::now())
                    .map_err(ToolError::Core)
            }
        }
    }
}

/// Render an engine answer as the standard MCP page.
///
/// Goes through `clove_core::ops::page_payload`, the same builder the file-path
/// `ops::list`/`ready`/`blocked`/`search` use, so a tier-served page and a
/// file-served one cannot come back in different shapes. The rows are always
/// [`Rows::Full`] here — the MCP tools' contract is full item objects, and
/// `fields` is a projection applied afterwards by [`shape::apply`], not a change
/// of source.
fn page_of(
    answer: &ListAnswer,
    window: Page,
    sort: &str,
    dir: &str,
    filters: Option<&Filters>,
) -> Value {
    let items: Vec<Value> = match &answer.rows {
        Rows::Full(rows) => rows
            .iter()
            .map(|row| {
                let mut obj = clove_core::view::frontmatter_object(&row.frontmatter);
                if let Some(blocked_by) = &row.blocked_by {
                    obj.insert("blocked_by".to_owned(), serde_json::json!(blocked_by));
                }
                Value::Object(obj)
            })
            .collect(),
        // Unreachable: every read here asks for `Projection::Full`. Rendering
        // the five columns rather than panicking keeps a future caller honest
        // without turning a projection mistake into a crashed tool call.
        Rows::Lean(rows) => rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "status": r.status,
                    "type": r.item_type,
                    "priority": r.priority,
                    "title": r.title,
                })
            })
            .collect(),
    };
    ops::page_payload(
        items,
        answer.total,
        window,
        sort,
        dir,
        filters,
        Some(answer.source.as_str()),
    )
}

/// Translate the wire shaping args into the shaping request.
fn shaping(a: &ShapeArgs) -> Shape {
    Shape {
        fields: a.fields.clone(),
        compact: a.compact,
    }
}

/// The MCP read window, through the shared limit contract.
fn window(offset: Option<u64>, limit: Option<u64>) -> Page {
    Page::new(
        offset.unwrap_or(0) as usize,
        limit.map(|n| n as usize),
        clove_core::view::defaults::MCP_LIMIT,
    )
}

fn parse_id(raw: &str) -> Result<CloveId, ToolError> {
    CloveId::new(raw).map_err(ToolError::Core)
}

/// `desc: true` → the shared `dir` word; `false`/absent → no direction (ascending).
fn dir_of(desc: Option<bool>) -> Option<&'static str> {
    desc.unwrap_or(false).then_some("desc")
}

/// The filter arguments as store filters.
fn filters_of(a: &FilterArgs) -> Result<Filters, ToolError> {
    let mut filters = Filters::parse_multi(
        &OneOrMany::values(&a.status),
        &OneOrMany::values(&a.item_type),
        &OneOrMany::values(&a.label),
        a.assignee.as_deref(),
        &Priorities::values(&a.priority),
        a.q.as_deref(),
    )?;
    filters.synced = a.synced;
    filters.parent = Filters::parse_parent(a.parent.as_deref())?;
    Ok(filters)
}

/// The requested ordering, through the same parser the CLI and web use.
fn order_of(a: &FilterArgs) -> Result<Order, ToolError> {
    Ok(Order::parse(a.sort.as_deref(), dir_of(a.desc))?)
}
