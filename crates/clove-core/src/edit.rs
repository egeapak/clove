//! The unified structured edit request shared by every write surface.
//!
//! [`EditRequest`] is the single, surface-agnostic description of a partial edit
//! to an existing item. The CLI's `KEY=VALUE` tokens, the web `PATCH` body, the
//! MCP `clove_edit` args, and the TUI edit form all converge on this one type, so
//! field validation and the status/closed invariant have exactly one
//! implementation. It is `Serialize`/`Deserialize` so it also rides the daemon
//! tarpc wire unchanged (like [`crate::ops::NewSpec`]).
//!
//! Graph edges (`deps`, `parent`, …) are deliberately **not** here: they need the
//! whole-store cycle/existence validation pipeline and stay as dedicated ops
//! ([`crate::ops::dep_add`], [`crate::ops::dep_remove`], [`crate::ops::set_parent`]).
//! `EditRequest` covers exactly the scalar + label surface, so applying it never
//! needs a graph build.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ops::set_status;
use crate::view::item_object;
use crate::{
    fields, CloveError, CloveId, ItemFrontmatter, ItemStatus, ItemStore, ItemType, Priority,
};

/// How to mutate the label set.
///
/// - [`LabelEdit::Delta`] is incremental — the CLI `labels+=`/`labels-=`, the web
///   `PUT /labels`, and the MCP `add_labels`/`remove_labels`. Removes are applied
///   first, then adds, so a label both removed and added in one request ends up
///   present.
/// - [`LabelEdit::Set`] replaces the whole label set — what an edit *form* submits.
///
/// Either way the resulting set is normalized, sorted, and deduped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelEdit {
    /// Incremental add/remove.
    Delta {
        #[serde(default)]
        add: Vec<String>,
        #[serde(default)]
        remove: Vec<String>,
    },
    /// Replace the entire label set.
    Set(Vec<String>),
}

/// A structured, partial edit of an existing item.
///
/// Every field is "absent → leave unchanged". A present value sets (or, for
/// [`EditRequest::assignee`], optionally clears). Field application order is
/// fixed: title, status, priority, type, assignee, labels — and the first
/// invalid field short-circuits with [`CloveError::InvalidField`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditRequest {
    /// `Some(t)` → set the title (rejected if empty/whitespace). `None` → leave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// `Some(b)` → replace the Markdown body (empty string clears it). `None` →
    /// leave. This is the only field that targets [`crate::Item::body`] rather
    /// than the frontmatter, so it is handled by [`apply_edit`], not
    /// [`EditRequest::apply_to_frontmatter`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,

    /// `Some(s)` → set status (maintaining the closed-timestamp invariant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ItemStatus>,

    /// `Some(p)` → set priority (already validated 0–4). `None` → leave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,

    /// `Some(t)` → set type. `None` → leave.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub item_type: Option<ItemType>,

    /// Tri-state assignee: outer `None` → leave; `Some(None)` → clear (nobody);
    /// `Some(Some(name))` → set (rejected if empty after trim — "clear" must be
    /// spelled `Some(None)`, never `Some(Some(""))`).
    ///
    /// The `double_option` deserializer makes an explicit JSON `null` map to
    /// `Some(None)` (clear) while an absent key maps to `None` (leave) — the
    /// distinction a plain `Option<Option<_>>` collapses.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub assignee: Option<Option<String>>,

    /// `Some(edit)` → mutate labels (see [`LabelEdit`]). `None` → leave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<LabelEdit>,
}

impl EditRequest {
    /// Whether this request would change nothing (every field absent).
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.body.is_none()
            && self.status.is_none()
            && self.priority.is_none()
            && self.item_type.is_none()
            && self.assignee.is_none()
            && self.labels.is_none()
    }

    /// Parse loose `KEY=VALUE` / `labels+=` / `labels-=` tokens into a structured
    /// request, preserving exactly the keys the CLI accepts today
    /// (`status|priority|type|assignee|title|labels+=|labels-=`). An empty
    /// `assignee=` clears the assignee (`Some(None)`).
    pub fn from_tokens(tokens: &[String]) -> Result<EditRequest, CloveError> {
        let mut req = EditRequest::default();
        let mut add: Vec<String> = Vec::new();
        let mut remove: Vec<String> = Vec::new();

        for token in tokens {
            let (raw_key, value) =
                token
                    .split_once('=')
                    .ok_or_else(|| CloveError::InvalidField {
                        field: "edit".to_owned(),
                        reason: format!("expected KEY=VALUE, got `{token}`"),
                    })?;

            if let Some(key) = raw_key.strip_suffix('+') {
                require_labels(key)?;
                add.push(value.to_owned());
                continue;
            }
            if let Some(key) = raw_key.strip_suffix('-') {
                require_labels(key)?;
                remove.push(value.to_owned());
                continue;
            }

            match raw_key {
                "status" => req.status = Some(ItemStatus::parse(value)?),
                "priority" => {
                    let n: u8 = value.parse().map_err(|_| CloveError::InvalidField {
                        field: "priority".to_owned(),
                        reason: format!("expected 0–4, got `{value}`"),
                    })?;
                    req.priority = Some(fields::parse_priority(n)?);
                }
                "type" => req.item_type = Some(fields::parse_type(value)?),
                "assignee" => {
                    req.assignee = Some(if value.trim().is_empty() {
                        None
                    } else {
                        Some(value.to_owned())
                    });
                }
                "title" => {
                    if value.trim().is_empty() {
                        return Err(CloveError::InvalidField {
                            field: "title".to_owned(),
                            reason: "title cannot be empty".to_owned(),
                        });
                    }
                    req.title = Some(value.to_owned());
                }
                other => {
                    return Err(CloveError::InvalidField {
                        field: other.to_owned(),
                        reason:
                            "unknown editable field (status|priority|type|assignee|title|labels+=|labels-=)"
                                .to_owned(),
                    })
                }
            }
        }

        if !add.is_empty() || !remove.is_empty() {
            req.labels = Some(LabelEdit::Delta { add, remove });
        }
        Ok(req)
    }

    /// Apply the frontmatter-targeting fields in place (everything except
    /// [`EditRequest::body`], which lives on the [`crate::Item`]). `now` stamps the
    /// closed timestamp on a transition to closed.
    pub fn apply_to_frontmatter(
        &self,
        fm: &mut ItemFrontmatter,
        now: DateTime<Utc>,
    ) -> Result<(), CloveError> {
        if let Some(title) = &self.title {
            if title.trim().is_empty() {
                return Err(CloveError::InvalidField {
                    field: "title".to_owned(),
                    reason: "title cannot be empty".to_owned(),
                });
            }
            fm.title = title.clone();
        }
        if let Some(status) = self.status {
            set_status(fm, status, now);
        }
        if let Some(priority) = self.priority {
            fm.priority = priority;
        }
        if let Some(item_type) = self.item_type {
            fm.item_type = item_type;
        }
        match &self.assignee {
            None => {}
            Some(None) => fm.assignee = None,
            Some(Some(name)) if name.trim().is_empty() => {
                return Err(CloveError::InvalidField {
                    field: "assignee".to_owned(),
                    reason: "use a clear (null) rather than an empty string".to_owned(),
                })
            }
            Some(Some(name)) => fm.assignee = Some(name.clone()),
        }
        if let Some(edit) = &self.labels {
            apply_label_edit(fm, edit)?;
        }
        Ok(())
    }
}

/// Deserialize a present field (including an explicit `null`) into the inner
/// `Option`, wrapped in `Some`; an absent field uses `#[serde(default)]` → `None`.
/// This is what lets `assignee: null` mean "clear" while an absent key means
/// "leave unchanged".
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

fn require_labels(key: &str) -> Result<(), CloveError> {
    if key == "labels" {
        Ok(())
    } else {
        Err(CloveError::InvalidField {
            field: key.to_owned(),
            reason: "only `labels` supports += / -=".to_owned(),
        })
    }
}

fn apply_label_edit(fm: &mut ItemFrontmatter, edit: &LabelEdit) -> Result<(), CloveError> {
    match edit {
        LabelEdit::Set(labels) => {
            fm.labels = fields::parse_labels(labels)?;
        }
        LabelEdit::Delta { add, remove } => {
            // Remove first, then add, so a label both removed and added survives.
            for raw in remove {
                let canonical = crate::normalize_label(raw)?;
                fm.labels.retain(|l| l != &canonical);
            }
            for raw in add {
                let canonical = crate::normalize_label(raw)?;
                if !fm.labels.contains(&canonical) {
                    fm.labels.push(canonical);
                }
            }
            fm.labels.sort();
            fm.labels.dedup();
        }
    }
    Ok(())
}

/// Ensure a non-empty body ends in exactly one trailing newline, matching the
/// canonical on-disk shape (the [`crate::write::FrontmatterWriter`] appends the
/// body bytes verbatim). An empty body stays empty. Applied only on the edit
/// path; creation stores the body as supplied.
pub fn normalize_body(body: &str) -> String {
    if body.is_empty() || body.ends_with('\n') {
        body.to_owned()
    } else {
        format!("{body}\n")
    }
}

/// Apply a structured [`EditRequest`] atomically and return the updated §7.4 item
/// object (byte-identical to [`crate::ops::edit`]/[`crate::ops::transition`]).
pub fn apply_edit(
    store: &ItemStore,
    id: &CloveId,
    req: &EditRequest,
    now: DateTime<Utc>,
) -> Result<Value, CloveError> {
    let mut item = store.get(id)?;
    req.apply_to_frontmatter(&mut item.frontmatter, now)?;
    if let Some(body) = &req.body {
        item.body = normalize_body(body);
    }
    let saved = store.update(&item, now)?;
    Ok(Value::Object(item_object(&saved)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ItemType, NewSpec};
    use serde_json::json;
    use tempfile::TempDir;

    fn store() -> (TempDir, ItemStore) {
        let dir = TempDir::new().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join(".clove/issues")).unwrap();
        (dir, ItemStore::new(root))
    }

    fn new_id(store: &ItemStore, title: &str) -> CloveId {
        let v = crate::ops::create(
            store,
            "proj",
            ItemType::Feature,
            NewSpec {
                title: title.to_owned(),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        CloveId::new(v["id"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn from_tokens_matches_legacy_fields() {
        let req = EditRequest::from_tokens(&[
            "priority=0".to_owned(),
            "assignee=alice".to_owned(),
            "labels+=urgent".to_owned(),
        ])
        .unwrap();
        assert_eq!(req.priority, Some(Priority(0)));
        assert_eq!(req.assignee, Some(Some("alice".to_owned())));
        assert_eq!(
            req.labels,
            Some(LabelEdit::Delta {
                add: vec!["urgent".to_owned()],
                remove: vec![],
            })
        );
        // Unknown field is rejected, like the legacy path.
        assert!(EditRequest::from_tokens(&["bogus=1".to_owned()]).is_err());
        // Empty assignee token clears (Some(None)).
        let cleared = EditRequest::from_tokens(&["assignee=".to_owned()]).unwrap();
        assert_eq!(cleared.assignee, Some(None));
    }

    #[test]
    fn apply_edit_sets_body_and_normalizes_newline() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        let v = apply_edit(
            &store,
            &id,
            &EditRequest {
                body: Some("a new body".to_owned()),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!(v["title"], "task");
        let reloaded = store.get(&id).unwrap();
        assert_eq!(reloaded.body, "a new body\n", "trailing newline added");
    }

    #[test]
    fn apply_edit_clears_assignee_with_some_none() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        apply_edit(
            &store,
            &id,
            &EditRequest {
                assignee: Some(Some("bob".to_owned())),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        let v = apply_edit(
            &store,
            &id,
            &EditRequest {
                assignee: Some(None),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert!(v["assignee"].is_null(), "assignee cleared");
        // Empty-string set is rejected (clear must be Some(None)).
        assert!(apply_edit(
            &store,
            &id,
            &EditRequest {
                assignee: Some(Some("  ".to_owned())),
                ..Default::default()
            },
            Utc::now()
        )
        .is_err());
    }

    #[test]
    fn label_set_replaces_whole_set() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        apply_edit(
            &store,
            &id,
            &EditRequest {
                labels: Some(LabelEdit::Delta {
                    add: vec!["a".to_owned(), "b".to_owned()],
                    remove: vec![],
                }),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        let v = apply_edit(
            &store,
            &id,
            &EditRequest {
                labels: Some(LabelEdit::Set(vec!["C".to_owned(), "a".to_owned()])),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!(
            v["labels"],
            json!(["a", "c"]),
            "set replaces + canonicalizes"
        );
    }

    #[test]
    fn apply_edit_preserves_closed_invariant() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        let closed = apply_edit(
            &store,
            &id,
            &EditRequest {
                status: Some(ItemStatus::Closed),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert_eq!(closed["status"], "closed");
        assert!(closed["closed"].is_string());
        let reopened = apply_edit(
            &store,
            &id,
            &EditRequest {
                status: Some(ItemStatus::Open),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        assert!(reopened["closed"].is_null(), "closed cleared on reopen");
    }

    #[test]
    fn empty_title_rejected() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        assert!(apply_edit(
            &store,
            &id,
            &EditRequest {
                title: Some("   ".to_owned()),
                ..Default::default()
            },
            Utc::now()
        )
        .is_err());
    }

    #[test]
    fn edit_request_serde_round_trips() {
        let req = EditRequest {
            title: Some("t".to_owned()),
            assignee: Some(None),
            labels: Some(LabelEdit::Set(vec!["x".to_owned()])),
            ..Default::default()
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: EditRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }
}
