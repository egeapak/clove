//! Tool argument structs. Each derives `Deserialize` (rmcp parses the call's
//! `arguments` into it) and `JsonSchema` (rmcp publishes the `inputSchema` in
//! `tools/list`). Schemars/serde are taken from rmcp's re-exports so the derive
//! versions match the macro-generated code exactly.

use rmcp::schemars::{self, JsonSchema};
use rmcp::serde::Deserialize;

/// Shared filter fields for `clove_ready` / `clove_list` / `clove_blocked`.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct FilterArgs {
    /// Filter by status (`open|in_progress|closed`).
    pub status: Option<String>,
    /// Filter by item type (`bug|feature|chore|docs|epic`).
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    /// Filter by a single label (case-insensitive).
    pub label: Option<String>,
    /// Filter by assignee.
    pub assignee: Option<String>,
    /// Filter by priority (0=highest .. 4).
    pub priority: Option<u8>,
    /// Max results (0 = no limit; default 50).
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct BlockedArgs {
    #[serde(flatten)]
    pub filter: FilterArgs,
    /// Also include items blocked only by dangling (missing) deps.
    pub include_warnings: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct ListArgs {
    #[serde(flatten)]
    pub filter: FilterArgs,
    /// Skip this many results.
    pub offset: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct IdArgs {
    /// The item id (e.g. `proj-7af3q2k9`).
    pub id: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct SearchArgs {
    /// The text to search for (case-insensitive substring).
    pub text: String,
    /// Max results (0 = no limit; default 50).
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct DepTreeArgs {
    /// The root item id.
    pub id: String,
    /// Maximum depth (default 5).
    pub depth: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct StatsArgs {
    /// Cap assignee/label breakdowns to the N highest (0 = no cap; default 10).
    pub top: Option<u64>,
    /// Skip the per-epic completion rollup.
    pub no_epics: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct StatusArgs {
    /// The item id.
    pub id: String,
    /// New status: `open`, `in_progress`, or `closed`.
    pub status: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
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
    /// Labels to add.
    pub add_labels: Option<Vec<String>>,
    /// Labels to remove.
    pub remove_labels: Option<Vec<String>>,
}

impl EditArgs {
    /// Translate the structured edit into `KEY=VALUE` assignment tokens for
    /// `clove_core::ops::edit` / the daemon `edit` RPC.
    pub fn to_tokens(&self) -> Vec<String> {
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

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct CommentArgs {
    /// The item id.
    pub id: String,
    /// The comment body.
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(crate = "rmcp::serde")]
pub struct DepAddArgs {
    /// The item that should depend on `dep_id`.
    pub id: String,
    /// The dependency target id.
    pub dep_id: String,
}
