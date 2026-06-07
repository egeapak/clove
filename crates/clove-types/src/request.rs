//! The create/edit request types shared by every write surface, plus the pure
//! frontmatter mutators they build on.
//!
//! [`NewSpec`] (create) and [`EditRequest`] (edit) are the single, surface-
//! agnostic descriptions of a mutation. The CLI's `KEY=VALUE` tokens, the web
//! `PATCH` body, the MCP `clove_edit` args, and the TUI edit form all converge on
//! these, so field validation and the status/closed invariant have exactly one
//! implementation. Both are `Serialize`/`Deserialize` so they also ride the
//! daemon tarpc wire unchanged.
//!
//! These types live in `clove-types` (no I/O), so they carry only the *pure*
//! application logic ([`EditRequest::apply_to_frontmatter`], [`set_status`], …).
//! The store-touching `apply_edit`/`create` orchestration lives in `clove-core`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{fields, normalize_label, CloveError, ItemFrontmatter, ItemStatus, ItemType, Priority};

/// A raw "new item" spec. Strings are parsed/validated by `clove_core::ops::create`;
/// the struct is serializable so it can also ride the daemon RPC wire unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewSpec {
    /// The item title (required, non-empty).
    pub title: String,
    /// `bug|feature|chore|docs|epic`; `None` → the caller's default type.
    pub item_type: Option<String>,
    /// Priority 0–4; `None` → [`Priority::DEFAULT`].
    pub priority: Option<u8>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    pub parent: Option<String>,
    pub assignee: Option<String>,
    pub body: Option<String>,
}

/// Apply a status transition to frontmatter, maintaining the closed-timestamp
/// invariant: set `closed` when moving to closed, clear it otherwise.
pub fn set_status(fm: &mut ItemFrontmatter, status: ItemStatus, now: DateTime<Utc>) {
    fm.status = status;
    match status {
        ItemStatus::Closed => {
            if fm.closed.is_none() {
                fm.closed = Some(now);
            }
        }
        ItemStatus::Open | ItemStatus::InProgress => fm.closed = None,
    }
}

/// Apply a list of `KEY=VALUE` (and `labels+=`/`labels-=`) edits to frontmatter.
///
/// Thin shim over the unified [`EditRequest`] path: the loose tokens are parsed
/// into an [`EditRequest`] and applied, so the CLI token surface and the
/// structured web/MCP/TUI surfaces share one validation implementation.
pub fn apply_assignments(
    fm: &mut ItemFrontmatter,
    assignments: &[String],
    now: DateTime<Utc>,
) -> Result<(), CloveError> {
    EditRequest::from_tokens(assignments)?.apply_to_frontmatter(fm, now)
}

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
    /// leave. This is the only field that targets the item body rather than the
    /// frontmatter, so it is handled by `clove_core::apply_edit`, not
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
                // Canonicalize so last-mention-wins compares the same label across
                // case/whitespace variants, matching the legacy in-order token path.
                let label = normalize_label(value)?;
                remove.retain(|l| l != &label);
                if !add.contains(&label) {
                    add.push(label);
                }
                continue;
            }
            if let Some(key) = raw_key.strip_suffix('-') {
                require_labels(key)?;
                let label = normalize_label(value)?;
                add.retain(|l| l != &label);
                if !remove.contains(&label) {
                    remove.push(label);
                }
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
    /// [`EditRequest::body`], which lives on the item). `now` stamps the closed
    /// timestamp on a transition to closed.
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
                let canonical = normalize_label(raw)?;
                fm.labels.retain(|l| l != &canonical);
            }
            for raw in add {
                let canonical = normalize_label(raw)?;
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
/// canonical on-disk shape (the frontmatter writer appends the body bytes
/// verbatim). An empty body stays empty. Applied only on the edit path; creation
/// stores the body as supplied.
pub fn normalize_body(body: &str) -> String {
    if body.is_empty() || body.ends_with('\n') {
        body.to_owned()
    } else {
        format!("{body}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CloveId, CURRENT_SCHEMA_VERSION};

    fn sample() -> ItemFrontmatter {
        ItemFrontmatter {
            schema: CURRENT_SCHEMA_VERSION,
            id: CloveId::new("proj-7AF3K2MN").unwrap(),
            title: "task".to_owned(),
            status: ItemStatus::Open,
            item_type: ItemType::Feature,
            priority: Priority::DEFAULT,
            created: "2026-06-02T10:00:00Z".parse().unwrap(),
            updated: "2026-06-02T10:00:00Z".parse().unwrap(),
            closed: None,
            assignee: None,
            parent: None,
            labels: Vec::new(),
            deps: Vec::new(),
            relates: Vec::new(),
            duplicates: Vec::new(),
            supersedes: Vec::new(),
            source_system: None,
            external_ref: None,
        }
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
        assert!(EditRequest::from_tokens(&["bogus=1".to_owned()]).is_err());
        let cleared = EditRequest::from_tokens(&["assignee=".to_owned()]).unwrap();
        assert_eq!(cleared.assignee, Some(None));
    }

    #[test]
    fn from_tokens_label_order_is_faithful() {
        // +x -x → net removed (last mention wins) even across case variants.
        let mut fm = sample();
        EditRequest::from_tokens(&["labels+=keep".to_owned()])
            .unwrap()
            .apply_to_frontmatter(&mut fm, Utc::now())
            .unwrap();
        EditRequest::from_tokens(&["labels+=Urgent".to_owned(), "labels-=urgent".to_owned()])
            .unwrap()
            .apply_to_frontmatter(&mut fm, Utc::now())
            .unwrap();
        assert_eq!(fm.labels, vec!["keep".to_owned()]);
        // -x +x → net present.
        EditRequest::from_tokens(&["labels-=keep".to_owned(), "labels+=keep".to_owned()])
            .unwrap()
            .apply_to_frontmatter(&mut fm, Utc::now())
            .unwrap();
        assert_eq!(fm.labels, vec!["keep".to_owned()]);
    }

    #[test]
    fn apply_clears_assignee_and_rejects_empty_set() {
        let mut fm = sample();
        fm.assignee = Some("bob".to_owned());
        EditRequest {
            assignee: Some(None),
            ..Default::default()
        }
        .apply_to_frontmatter(&mut fm, Utc::now())
        .unwrap();
        assert_eq!(fm.assignee, None);
        assert!(EditRequest {
            assignee: Some(Some("  ".to_owned())),
            ..Default::default()
        }
        .apply_to_frontmatter(&mut fm, Utc::now())
        .is_err());
    }

    #[test]
    fn label_set_replaces_whole_set() {
        let mut fm = sample();
        fm.labels = vec!["a".to_owned(), "b".to_owned()];
        EditRequest {
            labels: Some(LabelEdit::Set(vec!["C".to_owned(), "a".to_owned()])),
            ..Default::default()
        }
        .apply_to_frontmatter(&mut fm, Utc::now())
        .unwrap();
        assert_eq!(fm.labels, vec!["a".to_owned(), "c".to_owned()]);
    }

    #[test]
    fn set_status_maintains_closed_invariant() {
        let mut fm = sample();
        let now = Utc::now();
        set_status(&mut fm, ItemStatus::Closed, now);
        assert_eq!(fm.status, ItemStatus::Closed);
        assert_eq!(fm.closed, Some(now));
        set_status(&mut fm, ItemStatus::Open, now);
        assert_eq!(fm.closed, None);
    }

    #[test]
    fn empty_title_rejected() {
        let mut fm = sample();
        assert!(EditRequest {
            title: Some("   ".to_owned()),
            ..Default::default()
        }
        .apply_to_frontmatter(&mut fm, Utc::now())
        .is_err());
    }

    #[test]
    fn normalize_body_adds_single_trailing_newline() {
        assert_eq!(normalize_body("abc"), "abc\n");
        assert_eq!(normalize_body("abc\n"), "abc\n");
        assert_eq!(normalize_body(""), "");
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
