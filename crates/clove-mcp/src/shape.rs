//! Shaping for read-tool results: field projection and null/empty compaction.
//!
//! Applied only here, at the MCP boundary. `clove_core::view::frontmatter_object`
//! is untouched, so the CLI's renderer, the web DTOs, `export json`, and the
//! GitHub sync fingerprints — all of which serialize `ItemFrontmatter`
//! independently — keep the full-key shape they depend on.

use clove_core::view;
use serde_json::Value;

/// How a read result should be shaped before it goes on the wire.
#[derive(Debug, Clone, Default)]
pub struct Shape {
    /// Return only these keys per item. Unknown names are ignored.
    pub fields: Option<Vec<String>>,
    /// Drop null/empty-list keys. `None` means "compact unless `fields` was
    /// given" — an explicit projection is already a deliberate key list, so it
    /// is honoured literally.
    pub compact: Option<bool>,
}

impl Shape {
    fn compact_enabled(&self) -> bool {
        self.compact.unwrap_or(self.fields.is_none())
    }
}

/// Keys that are never useful to an agent and are dropped whenever compaction
/// is on. `schema` is a per-file migration marker, not item data.
const NOISE: &[&str] = &["schema"];

/// Shape a read result in place.
///
/// Handles both result forms the read tools produce: a
/// `{total, returned, offset, items: [...]}` page, where only the elements are
/// shaped and the envelope counts are preserved, and a single object (`show`),
/// which is shaped directly.
///
/// Compaction recurses into nested objects and arrays, which is why `dep_tree`
/// is *not* routed through here: its published schema requires `children` on
/// every node, and recursion would strip it from every leaf.
pub fn apply(value: Value, shape: &Shape) -> Value {
    match value {
        Value::Object(mut obj) if obj.contains_key("items") => {
            if let Some(Value::Array(items)) = obj.remove("items") {
                let shaped: Vec<Value> = items.into_iter().map(|i| one(i, shape)).collect();
                obj.insert("items".to_owned(), Value::Array(shaped));
            }
            Value::Object(obj)
        }
        other => one(other, shape),
    }
}

fn one(value: Value, shape: &Shape) -> Value {
    let Value::Object(mut obj) = value else {
        return value;
    };
    match &shape.fields {
        Some(fields) => {
            let projected = view::project(obj, fields);
            // A projection is taken literally: `fields: ["assignee"]` on an
            // unassigned item still yields `{"assignee": null}`, so the caller
            // can tell "unset" from "not requested". Compaction applies only if
            // it was asked for explicitly.
            if shape.compact == Some(true) {
                Value::Object(view::compact(projected))
            } else {
                Value::Object(projected)
            }
        }
        None if shape.compact_enabled() => {
            for key in NOISE {
                obj.remove(*key);
            }
            Value::Object(view::compact(obj))
        }
        None => Value::Object(obj),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row() -> Value {
        json!({
            "id": "proj-0000000A", "title": "t", "status": "open", "type": "bug",
            "priority": 2, "schema": 1, "assignee": null, "parent": null,
            "closed": null, "labels": [], "deps": [], "ready": false,
        })
    }

    #[test]
    fn page_envelope_survives_shaping() {
        let page = json!({ "total": 7, "returned": 1, "offset": 0, "items": [row()] });
        let out = apply(page, &Shape::default());
        assert_eq!(out["total"], 7);
        assert_eq!(out["returned"], 1);
        assert_eq!(out["offset"], 0);
        assert!(out["items"][0].get("assignee").is_none(), "row compacted");
    }

    #[test]
    fn compact_is_the_default_and_keeps_false() {
        let out = apply(row(), &Shape::default());
        assert!(out.get("assignee").is_none());
        assert!(out.get("labels").is_none());
        assert!(out.get("schema").is_none(), "migration marker dropped");
        assert_eq!(out["ready"], false, "`false` is an answer, not an absence");
        assert_eq!(out["priority"], 2);
    }

    #[test]
    fn compact_false_restores_the_full_shape() {
        let shape = Shape {
            fields: None,
            compact: Some(false),
        };
        let out = apply(row(), &shape);
        assert!(out["assignee"].is_null());
        assert_eq!(out["labels"], json!([]));
        assert_eq!(out["schema"], 1);
    }

    #[test]
    fn fields_are_honoured_literally_unless_compact_is_explicit() {
        let shape = Shape {
            fields: Some(vec!["id".into(), "assignee".into()]),
            compact: None,
        };
        let out = apply(row(), &shape);
        assert_eq!(out.as_object().unwrap().len(), 2);
        assert!(
            out["assignee"].is_null(),
            "an explicit ask returns the null"
        );

        let both = Shape {
            fields: Some(vec!["id".into(), "assignee".into()]),
            compact: Some(true),
        };
        let out = apply(row(), &both);
        assert_eq!(out.as_object().unwrap().len(), 1, "compaction composes");
        assert_eq!(out["id"], "proj-0000000A");
    }

    #[test]
    fn unknown_field_names_are_ignored() {
        let shape = Shape {
            fields: Some(vec!["id".into(), "nonexistent".into()]),
            compact: None,
        };
        let out = apply(row(), &shape);
        assert_eq!(out.as_object().unwrap().len(), 1);
    }
}
