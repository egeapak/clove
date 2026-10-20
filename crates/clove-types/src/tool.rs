//! The tool surface's argument types: one struct per tool of the clove tool
//! surface (MCP `tools/call` arguments and the plugin host IPC params).
//!
//! These are plain serde types, so the MCP server (`clove-mcp`, AGPL) and the
//! plugin SDK client (`clove-plugin`, MIT OR Apache-2.0) share one definition
//! and the two transports cannot drift. `JsonSchema` derives sit behind the
//! optional `schemars` feature: only `clove-mcp` enables it, to publish each
//! tool's `inputSchema` (doc comments become the schema descriptions, so keep
//! them written for the agent reading `tools/list`).
//!
//! The translation into store queries (`Filters`, `Order`) lives with the
//! engine in `clove-service`, which owns those AGPL types.

use serde::{Deserialize, Serialize};

use crate::{CloveError, EditRequest};

/// Result-shaping options shared by the read tools.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ShapeArgs {
    /// Return only these keys per item (e.g. ["id","title","status"]). Unknown
    /// names are ignored. Cuts list payloads by roughly two thirds.
    pub fields: Option<Vec<String>>,
    /// Omit null and empty-list keys. Defaults to true unless `fields` is given.
    pub compact: Option<bool>,
}

/// One value or a list of them.
///
/// The filter arguments accept `"open"` and `["open","in_progress"]` alike, so
/// every agent already calling these tools with a single string is unaffected —
/// the published `inputSchema` gains an `anyOf` branch rather than changing
/// type. `#[serde(untagged)]` is what makes the string form keep working; the
/// `JsonSchema` derive mirrors it into the schema agents read.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    /// The values as a list; `None` (the argument was absent) is unconstrained.
    pub fn values(this: &Option<OneOrMany>) -> Vec<String> {
        match this {
            None => Vec::new(),
            Some(OneOrMany::One(v)) => vec![v.clone()],
            Some(OneOrMany::Many(v)) => v.clone(),
        }
    }
}

/// One priority or a list of them (`2` or `[0,1]`). Numeric, as it always was.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum Priorities {
    One(u8),
    Many(Vec<u8>),
}

impl Priorities {
    /// The values as priority words; `None` is unconstrained.
    pub fn values(this: &Option<Priorities>) -> Vec<String> {
        match this {
            None => Vec::new(),
            Some(Priorities::One(p)) => vec![p.to_string()],
            Some(Priorities::Many(v)) => v.iter().map(u8::to_string).collect(),
        }
    }
}

/// Shared filter fields for `clove_ready` / `clove_list` / `clove_blocked`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct FilterArgs {
    /// Filter by status (`open|in_progress|closed`). A list matches any of them.
    pub status: Option<OneOrMany>,
    /// Filter by item type (`bug|feature|chore|docs|epic`). A list matches any
    /// of them.
    #[serde(rename = "type")]
    pub item_type: Option<OneOrMany>,
    /// Filter by label (case-insensitive). A list requires **all** of them.
    pub label: Option<OneOrMany>,
    /// Filter by assignee.
    pub assignee: Option<String>,
    /// Filter by priority (0=highest .. 4). A list matches any of them.
    pub priority: Option<Priorities>,
    /// Keep only items whose id, title, or labels contain this text
    /// (case-insensitive). A filter, not a search: it never reads the body.
    pub q: Option<String>,
    /// `true`: only items linked to an external tracker (any `external_ref`,
    /// e.g. a GitHub `gh-12`); `false`: only items with none.
    pub synced: Option<bool>,
    /// Only the direct children of this item id (e.g. `proj-7af3q2k9`).
    pub parent: Option<String>,
    /// Sort by `rank|priority|created|updated|id|status|type`. Default `rank`:
    /// priority, then dependency order, then id.
    pub sort: Option<String>,
    /// Reverse the sort order (newest-first, lowest-priority-first, ...).
    pub desc: Option<bool>,
    /// Max results (0 = no limit; default 50).
    pub limit: Option<u64>,
    /// Skip this many results.
    pub offset: Option<u64>,
    #[serde(flatten)]
    pub shape: ShapeArgs,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BlockedArgs {
    #[serde(flatten)]
    pub filter: FilterArgs,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ListArgs {
    #[serde(flatten)]
    pub filter: FilterArgs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct IdArgs {
    /// The item id (e.g. `proj-7af3q2k9`).
    pub id: String,
    #[serde(flatten)]
    pub shape: ShapeArgs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SearchArgs {
    /// The text to search for (case-insensitive substring).
    pub text: String,
    /// Sort by `rank|priority|created|updated|id|status|type`. Omitted, hits are
    /// ranked by relevance (title, then label, then body); naming a field
    /// replaces that ranking entirely.
    pub sort: Option<String>,
    /// Reverse the order (or, with no `sort`, the relevance ranking).
    pub desc: Option<bool>,
    /// Max results (0 = no limit; default 50).
    pub limit: Option<u64>,
    /// Skip this many results.
    pub offset: Option<u64>,
    #[serde(flatten)]
    pub shape: ShapeArgs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CommentsArgs {
    /// The item id (e.g. `proj-7af3q2k9`).
    pub id: String,
    /// Max comments, keeping the most recent (0 = no limit; default 50).
    pub limit: Option<u64>,
    /// Skip this many of the *newest* comments, to page back through older
    /// ones. Named for its direction: unlike `offset` on the list tools, this
    /// window is anchored at the newest end, not the start.
    pub skip_newest: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DepTreeArgs {
    /// The root item id.
    pub id: String,
    /// Maximum depth (0 = no limit; default 5).
    pub depth: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct StatsArgs {
    /// Cap assignee/label breakdowns to the N highest (0 = no cap; default 10).
    pub top: Option<u64>,
    /// Skip the per-epic completion rollup.
    pub no_epics: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct NewArgs {
    /// The item title (required).
    pub title: String,
    /// `bug|feature|chore|docs|epic`; defaults to the repo's configured type.
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    /// Priority 0 (highest) .. 4 (default 2).
    pub priority: Option<u8>,
    /// Labels to attach (case-insensitive).
    pub labels: Option<Vec<String>>,
    /// Ids this item hard-depends on.
    pub deps: Option<Vec<String>>,
    /// Parent item id (for epics).
    pub parent: Option<String>,
    /// Assignee.
    pub assignee: Option<String>,
    /// Markdown body.
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct StatusArgs {
    /// The item id.
    pub id: String,
    /// New status: `open`, `in_progress`, or `closed`.
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct EditArgs {
    /// The item id.
    pub id: String,
    /// New status (`open|in_progress|closed`).
    pub status: Option<String>,
    /// New priority (0..4).
    pub priority: Option<u8>,
    /// New type (`bug|feature|chore|docs|epic`).
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    /// New title.
    pub title: Option<String>,
    /// New assignee (empty string clears it).
    pub assignee: Option<String>,
    /// New Markdown body (replaces the body; empty string clears it).
    pub body: Option<String>,
    /// Labels to add.
    pub add_labels: Option<Vec<String>>,
    /// Labels to remove.
    pub remove_labels: Option<Vec<String>>,
}

impl EditArgs {
    /// Translate the structured edit into the unified [`EditRequest`]. The
    /// scalar/label fields ride through `from_tokens` (so the empty-assignee →
    /// clear and label add/remove semantics match the CLI exactly), and the body
    /// is attached directly (the one field tokens don't carry).
    pub fn to_request(&self) -> Result<EditRequest, CloveError> {
        let mut req = EditRequest::from_tokens(&self.to_tokens())?;
        req.body = self.body.clone();
        Ok(req)
    }

    /// The scalar/label fields as `KEY=VALUE` assignment tokens (body excluded —
    /// it is multi-line and carried on the [`EditRequest`] directly).
    fn to_tokens(&self) -> Vec<String> {
        let mut t = Vec::new();
        if let Some(s) = &self.status {
            t.push(format!("status={s}"));
        }
        if let Some(p) = self.priority {
            t.push(format!("priority={p}"));
        }
        if let Some(ty) = &self.item_type {
            t.push(format!("type={ty}"));
        }
        if let Some(title) = &self.title {
            t.push(format!("title={title}"));
        }
        if let Some(a) = &self.assignee {
            t.push(format!("assignee={a}"));
        }
        for l in self.add_labels.iter().flatten() {
            t.push(format!("labels+={l}"));
        }
        for l in self.remove_labels.iter().flatten() {
            t.push(format!("labels-={l}"));
        }
        t
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CommentArgs {
    /// The item id.
    pub id: String,
    /// The comment body.
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DepAddArgs {
    /// The item that should depend on `dep_id`.
    pub id: String,
    /// The dependency target id.
    pub dep_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SetParentArgs {
    /// The item whose parent to set.
    pub id: String,
    /// The new parent id, or null/omitted to clear the parent.
    pub parent: Option<String>,
}

// ---- Store tier (host IPC `store.*` methods, PLUGIN_SYSTEM.md §6.6) --------
//
// Lower-level than the tools above: what an importer or a sync engine needs to
// mirror another tracker faithfully (whole items with their own timestamps and
// `external_ref`, comments with their original author and time). Every write
// still runs in the host through clove's validated write path. Not exposed over
// MCP.

/// A whole item on the wire: its frontmatter and Markdown body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemPayload {
    pub frontmatter: crate::ItemFrontmatter,
    pub body: String,
}

impl From<crate::Item> for ItemPayload {
    fn from(item: crate::Item) -> Self {
        ItemPayload {
            frontmatter: item.frontmatter,
            body: item.body,
        }
    }
}

impl From<ItemPayload> for crate::Item {
    fn from(p: ItemPayload) -> Self {
        crate::Item {
            frontmatter: p.frontmatter,
            body: p.body,
        }
    }
}

/// An item file that failed to parse during a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanErrorPayload {
    pub path: String,
    pub message: String,
}

/// `store.scan`: every parsable item, plus the files that failed to parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanPayload {
    pub items: Vec<ItemPayload>,
    pub errors: Vec<ScanErrorPayload>,
}

/// `store.scan_frontmatter`: every parsable frontmatter, plus the failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontmatterScanPayload {
    pub frontmatters: Vec<crate::ItemFrontmatter>,
    pub errors: Vec<ScanErrorPayload>,
}

/// One comment with its original author and time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentPayload {
    /// The comment's stable name within its item (its file name); what
    /// `store.update_comment` / `store.delete_comment` take.
    #[serde(default)]
    pub key: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub author: String,
    pub body: String,
    /// When the comment's body was last written, if the store knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<chrono::DateTime<chrono::Utc>>,
}

/// `store.get`, `store.comments`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreIdParams {
    pub id: String,
}

/// `store.create_item`: write a new item exactly as given (its id from
/// `store.new_id`, its own timestamps). Validated like `create`; refuses an id
/// that already exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateItemParams {
    pub item: ItemPayload,
}

/// `store.apply_edit`: the unified edit. The writer (the daemon, or the host
/// when no daemon runs) stamps `updated`, as for every other write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyEditParams {
    pub id: String,
    pub request: EditRequest,
}

/// `store.dep_add` / `store.dep_remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreDepParams {
    pub id: String,
    pub dep: String,
}

/// `store.link_external`: set `source_system` + `external_ref` on an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkExternalParams {
    pub id: String,
    pub source_system: String,
    pub external_ref: String,
}

/// `store.update_comment`: replace the body of an existing comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCommentParams {
    pub id: String,
    pub key: String,
    pub body: String,
}

/// `store.delete_comment`: remove an existing comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteCommentParams {
    pub id: String,
    pub key: String,
}

/// `store.add_comment`: append a comment with its original author and time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddCommentParams {
    pub id: String,
    pub author: String,
    pub body: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}
