//! Store-level orchestration of a structured edit.
//!
//! The [`EditRequest`] type and its pure application logic live in
//! `clove_types::request`; this module is the thin store-touching layer:
//! [`apply_edit`] loads the item, applies the request, normalizes the body, and
//! writes it back atomically, returning the §7.4 item JSON.

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::view::item_object;
use crate::{CloveError, CloveId, EditRequest, ItemStore};
use clove_types::normalize_body;

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
    use crate::{CloveError, ItemType};
    use clove_types::{ItemStatus, LabelEdit, NewSpec};
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
    fn from_tokens_label_order_is_faithful_through_store() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        apply_edit(
            &store,
            &id,
            &EditRequest::from_tokens(&["labels+=keep".to_owned()]).unwrap(),
            Utc::now(),
        )
        .unwrap();
        let v = apply_edit(
            &store,
            &id,
            &EditRequest::from_tokens(&["labels+=Urgent".to_owned(), "labels-=urgent".to_owned()])
                .unwrap(),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(v["labels"], json!(["keep"]), "add then remove nets removed");
    }

    #[test]
    fn empty_title_rejected() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        let err = apply_edit(
            &store,
            &id,
            &EditRequest {
                title: Some("   ".to_owned()),
                ..Default::default()
            },
            Utc::now(),
        );
        assert!(matches!(err, Err(CloveError::InvalidField { .. })));
    }
}
