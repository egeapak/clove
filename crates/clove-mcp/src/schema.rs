//! The published output schemas for the read tools that return a page.
//!
//! MCP lets a tool advertise an `outputSchema` in `tools/list`; a client can
//! then generate types for the result and validate what it gets, instead of
//! discovering the shape by calling the tool. clove's page payload
//! (`{total, returned, offset, limit, sort, dir, items, …}`) had no published
//! schema on *any* surface and no `outputSchema` here (read-path roadmap §7),
//! so `limit: 0` meaning "unlimited" — the kind of thing a schema exists to say
//! — was folklore.
//!
//! The schema text below is the same document published under
//! `docs/json-schema/v1/`; [`tests`] parses both and asserts they are equal, so
//! the advertised schema and the published file cannot drift. (The text is
//! duplicated rather than `include_str!`d because `docs/` is outside this
//! crate's package: a published `clove-mcp` would not carry the file.)
//!
//! Scope, deliberately: only the tools whose result is a *page*
//! (`clove_list`/`clove_ready`/`clove_blocked`/`clove_search`, and
//! `clove_comments` with its newest-anchored window) advertise a schema. An
//! `outputSchema` is a promise the payload must keep on every call — the MCP
//! contract is that `structuredContent` validates against it — so a tool whose
//! result this crate does not fully own (`clove_show` and the write tools return
//! whatever the daemon or `clove-core` built; `clove_stats` is the whole
//! analytics report; `clove_dep_tree` is a recursive node the CLI and the engine
//! render with different key sets) advertises nothing rather than a schema that
//! is right most of the time.
//!
//! Errors are unaffected: a failed tool call returns `isError` with a text
//! message and NO `structuredContent`, which is the shape MCP defines for
//! errors — an `outputSchema` constrains the structured result when there is
//! one, it does not require one on the error path.

use std::sync::{Arc, LazyLock};

use rmcp::model::JsonObject;

/// `docs/json-schema/v1/mcp-item-page.json`.
pub const ITEM_PAGE_JSON: &str = r##"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://clove.dev/schema/v1/mcp-item-page.json",
  "title": "clove MCP item page",
  "description": "The result of the MCP read tools that return a list: clove_list, clove_ready, clove_blocked, clove_search. This is the payload the server puts in `structuredContent` (and, byte-identically, in `content[0].text` for clients that do not read structured results), and it is what those tools advertise as their `outputSchema` in tools/list. It is the CLI's list envelope flattened: an MCP tool result has no `{v, ok, data, _meta}` wrapper, so the keys the CLI reports under `_meta` are plain keys beside `items` here.",
  "type": "object",
  "required": ["total", "returned", "offset", "limit", "sort", "dir", "items"],
  "properties": {
    "total": {
      "description": "Matches BEFORE the window — always the full count, never the page size. `returned < total` means there is more to page through.",
      "type": "integer",
      "minimum": 0
    },
    "returned": {
      "description": "Elements in `items`.",
      "type": "integer",
      "minimum": 0
    },
    "offset": {
      "description": "Rows skipped from the start of the ordered result.",
      "type": "integer",
      "minimum": 0
    },
    "limit": {
      "description": "The page size in force, in the same encoding callers pass in: 0 means UNLIMITED, not \"empty page\". Echoed so a caller can read the effective cap rather than assume the MCP default (50).",
      "type": "integer",
      "minimum": 0
    },
    "sort": {
      "description": "The ordering field in force. `relevance` is clove_search's default and cannot be requested on the other three.",
      "enum": ["rank", "priority", "created", "updated", "id", "status", "type", "relevance"]
    },
    "dir": { "enum": ["asc", "desc"] },
    "source": {
      "description": "Which read tier answered: a running cloved, the SQLite index, or a file scan. The `_meta.source` of the CLI and web, carried as a plain key because this payload has no `_meta`.",
      "enum": ["daemon", "index", "files"]
    },
    "filters": {
      "title": "the applied filter set",
      "description": "The parsed, canonicalized filters, echoed so a caller can read back what was applied rather than assume its input survived. Absent on clove_search, which takes no field filters — an empty object there would advertise a surface the tool does not have. Any-of within a field, all-of across fields; `labels` is all-of within the field too. An empty array does not constrain.",
      "type": "object",
      "properties": {
        "status": { "type": "array", "items": { "enum": ["open", "in_progress", "closed"] } },
        "type": { "type": "array", "items": { "enum": ["bug", "feature", "chore", "docs", "epic"] } },
        "priority": {
          "type": "array",
          "items": { "type": "integer", "minimum": 0, "maximum": 4 }
        },
        "labels": { "type": "array", "items": { "type": "string" } },
        "assignee": { "type": ["string", "null"] },
        "q": {
          "description": "Case-insensitive substring over id/title/labels (NOT bodies — that is clove_search).",
          "type": ["string", "null"]
        }
      },
      "additionalProperties": false
    },
    "items": {
      "description": "The requested page, already windowed. Elements are item objects whose KEY SET depends on the request: the read tools compact by default (null and empty-list keys, plus the `schema` migration marker, are omitted — a missing `assignee` means unset, never unknown), `fields` projects to exactly the keys asked for, and `compact: false` restores every key. The full key set is item.json; no key is required here because any of them can be projected away.",
      "type": "array",
      "items": { "type": "object" }
    }
  },
  "additionalProperties": false
}"##;

/// `docs/json-schema/v1/mcp-comment-page.json`.
pub const COMMENT_PAGE_JSON: &str = r##"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://clove.dev/schema/v1/mcp-comment-page.json",
  "title": "clove MCP comment page",
  "description": "The result of the clove_comments MCP tool: one window of an item's comment thread, oldest first. This is the payload the server puts in `structuredContent` (and, byte-identically, in `content[0].text`), and what the tool advertises as its `outputSchema` in tools/list. The window is anchored at the NEWEST end — `limit` keeps the most recent comments and `skip_newest` pages back into older ones — which is why the skip key is not called `offset`.",
  "type": "object",
  "required": ["total", "returned", "skip_newest", "limit", "items"],
  "properties": {
    "total": {
      "description": "Comments on the item BEFORE the window.",
      "type": "integer",
      "minimum": 0
    },
    "returned": { "type": "integer", "minimum": 0 },
    "skip_newest": {
      "description": "How many of the newest comments were skipped.",
      "type": "integer",
      "minimum": 0
    },
    "limit": {
      "description": "The page size in force; 0 means unlimited, as everywhere else.",
      "type": "integer",
      "minimum": 0
    },
    "items": {
      "description": "The windowed thread, oldest first.",
      "type": "array",
      "items": {
        "type": "object",
        "required": ["author", "timestamp", "body"],
        "properties": {
          "author": { "type": "string" },
          "timestamp": {
            "description": "RFC3339 UTC with whole seconds — the one canonical spelling every surface writes.",
            "type": "string"
          },
          "body": { "type": "string" }
        },
        "additionalProperties": false
      }
    }
  },
  "additionalProperties": false
}"##;

/// Parse a schema document into the object rmcp advertises. A malformed schema
/// is a bug in this file, caught by the tests below (and by every tool call in
/// every integration test, since this runs at first use).
fn parse(text: &str) -> Arc<JsonObject> {
    let value: serde_json::Value = serde_json::from_str(text).expect("schema is valid JSON");
    match value {
        serde_json::Value::Object(map) => Arc::new(map),
        _ => panic!("schema must be a JSON object"),
    }
}

static ITEM_PAGE: LazyLock<Arc<JsonObject>> = LazyLock::new(|| parse(ITEM_PAGE_JSON));
static COMMENT_PAGE: LazyLock<Arc<JsonObject>> = LazyLock::new(|| parse(COMMENT_PAGE_JSON));

/// The `outputSchema` for `clove_list` / `clove_ready` / `clove_blocked` /
/// `clove_search`.
pub fn item_page() -> Arc<JsonObject> {
    ITEM_PAGE.clone()
}

/// The `outputSchema` for `clove_comments`.
pub fn comment_page() -> Arc<JsonObject> {
    COMMENT_PAGE.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Read a published schema from `docs/json-schema/v1/`.
    fn published(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/json-schema/v1")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        serde_json::from_str(&text).expect("published schema is valid JSON")
    }

    /// The advertised schema and the published file are the same document.
    /// Without this, `tools/list` could promise a shape the docs contradict.
    #[test]
    fn advertised_schemas_match_the_published_files() {
        let item: Value = serde_json::from_str(ITEM_PAGE_JSON).unwrap();
        assert_eq!(item, published("mcp-item-page.json"));
        let comment: Value = serde_json::from_str(COMMENT_PAGE_JSON).unwrap();
        assert_eq!(comment, published("mcp-comment-page.json"));
    }

    /// Both schemas are objects describing an object, as MCP requires of an
    /// `outputSchema`, and they actually constrain the page keys.
    #[test]
    fn schemas_are_object_schemas_that_constrain_the_page() {
        for schema in [item_page(), comment_page()] {
            assert_eq!(schema.get("type").and_then(Value::as_str), Some("object"));
            let required: Vec<&str> = schema["required"]
                .as_array()
                .expect("required list")
                .iter()
                .filter_map(Value::as_str)
                .collect();
            for key in ["total", "returned", "limit", "items"] {
                assert!(
                    required.contains(&key),
                    "{key} must be required: {required:?}"
                );
            }
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&Value::Bool(false))
            );
        }
    }
}
