//! The store tier of the host IPC channel (`store.*`, `PLUGIN_SYSTEM.md` §6.6).
//!
//! The tools in [`crate::Engine`] are the agent-level surface. An importer or a
//! sync engine needs a little more: whole items with their own timestamps and
//! `external_ref`, freshly minted ids, and comments with their original author
//! and time. [`StoreService`] offers exactly that over the store, with every
//! write going through clove's validated write path:
//!
//! - `create_item` → [`ItemStore::restore_item`] (the same field checks as
//!   `create`; never overwrites),
//! - `apply_edit` → [`clove_core::apply_edit`],
//! - `dep_add`/`dep_remove` → [`clove_core::ops`] (cycle-checked),
//! - `link_external` → [`ItemStore::update_with`] under the store write lock,
//! - `add_comment` → [`clove_core::comments::add_comment_at`].
//!
//! Like the importers before it, this tier writes in the calling process; it
//! does not route through `cloved`.

use camino::Utf8PathBuf;
use clove_core::comments::{add_comment_at, list_comments};
use clove_core::{ItemStore, RestoreOutcome, ScanError};
use clove_types::tool::{
    AddCommentParams, ApplyEditParams, CommentPayload, CreateItemParams, FrontmatterScanPayload,
    ItemPayload, LinkExternalParams, ScanErrorPayload, ScanPayload, StoreDepParams, StoreIdParams,
};
use clove_types::{CloveError, CloveId, Item};
use serde_json::{json, Value};

use crate::ToolError;

/// The store tier over one repository.
#[derive(Clone)]
pub struct StoreService {
    store: ItemStore,
    id_prefix: String,
}

impl StoreService {
    pub fn new(repo_root: Utf8PathBuf, id_prefix: String) -> Self {
        StoreService {
            store: ItemStore::new(repo_root),
            id_prefix,
        }
    }

    /// Every item (frontmatter + body), plus the files that failed to parse.
    pub fn scan(&self) -> Result<Value, ToolError> {
        let (items, errors) = self.store.scan()?;
        to_value(ScanPayload {
            items: items.into_iter().map(ItemPayload::from).collect(),
            errors: errors.iter().map(scan_error).collect(),
        })
    }

    /// Every frontmatter, plus the files that failed to parse.
    pub fn scan_frontmatter(&self) -> Result<Value, ToolError> {
        let (frontmatters, errors) = self.store.scan_frontmatter()?;
        to_value(FrontmatterScanPayload {
            frontmatters,
            errors: errors.iter().map(scan_error).collect(),
        })
    }

    /// One whole item.
    pub fn get(&self, p: StoreIdParams) -> Result<Value, ToolError> {
        let item = self.store.get(&id(&p.id)?)?;
        to_value(ItemPayload::from(item))
    }

    /// A fresh id with the repository's prefix, not yet used by any item.
    pub fn new_id(&self) -> Result<Value, ToolError> {
        let id = clove_types::new_id(&self.id_prefix, self.store.issues_dir())?;
        Ok(json!({ "id": id }))
    }

    /// Write a new item exactly as given. Refuses an id that already exists.
    pub fn create_item(&self, p: CreateItemParams) -> Result<Value, ToolError> {
        let item = Item::from(p.item);
        match self.store.restore_item(&item, false)? {
            RestoreOutcome::Skipped => Err(ToolError::Core(CloveError::InvalidField {
                field: "id".to_owned(),
                reason: format!("item `{}` already exists", item.frontmatter.id),
            })),
            _ => Ok(json!({ "id": item.frontmatter.id })),
        }
    }

    /// The unified edit (`clove_core::apply_edit`), stamped `now`.
    pub fn apply_edit(&self, p: ApplyEditParams) -> Result<Value, ToolError> {
        Ok(clove_core::apply_edit(
            &self.store,
            &id(&p.id)?,
            &p.request,
            p.now,
        )?)
    }

    /// Add a hard dependency (cycle-checked).
    pub fn dep_add(&self, p: StoreDepParams) -> Result<Value, ToolError> {
        Ok(clove_core::ops::dep_add(
            &self.store,
            &id(&p.id)?,
            &id(&p.dep)?,
            p.now,
        )?)
    }

    /// Remove a hard dependency.
    pub fn dep_remove(&self, p: StoreDepParams) -> Result<Value, ToolError> {
        Ok(clove_core::ops::dep_remove(
            &self.store,
            &id(&p.id)?,
            &id(&p.dep)?,
            p.now,
        )?)
    }

    /// Link an item to an external tracker (`source_system` + `external_ref`).
    pub fn link_external(&self, p: LinkExternalParams) -> Result<Value, ToolError> {
        let saved = self.store.update_with(&id(&p.id)?, p.now, |item| {
            item.frontmatter.source_system = Some(p.source_system.clone());
            item.frontmatter.external_ref = Some(p.external_ref.clone());
            Ok(())
        })?;
        Ok(json!({ "updated": saved.frontmatter.updated }))
    }

    /// An item's comments, oldest first, with author and time.
    pub fn comments(&self, p: StoreIdParams) -> Result<Value, ToolError> {
        let comments = list_comments(self.store.issues_dir(), &id(&p.id)?)?;
        to_value(
            comments
                .into_iter()
                .map(|c| CommentPayload {
                    timestamp: c.timestamp,
                    author: c.author,
                    body: c.body,
                })
                .collect::<Vec<_>>(),
        )
    }

    /// Append a comment with its original author and time.
    pub fn add_comment(&self, p: AddCommentParams) -> Result<Value, ToolError> {
        add_comment_at(
            self.store.issues_dir(),
            &id(&p.id)?,
            &p.author,
            &p.body,
            p.timestamp,
        )?;
        Ok(json!({}))
    }
}

fn id(raw: &str) -> Result<CloveId, ToolError> {
    Ok(CloveId::new(raw)?)
}

fn scan_error(e: &ScanError) -> ScanErrorPayload {
    ScanErrorPayload {
        path: e.path().to_string(),
        message: e.to_string(),
    }
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, ToolError> {
    serde_json::to_value(value)
        .map_err(|e| ToolError::Usage(format!("cannot encode the result: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use clove_types::tool::ItemPayload;
    use clove_types::EditRequest;

    fn service() -> (tempfile::TempDir, StoreService) {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join(".clove/issues")).unwrap();
        (dir, StoreService::new(root, "proj".to_owned()))
    }

    fn item(svc: &StoreService, title: &str) -> ItemPayload {
        let id = svc.new_id().unwrap()["id"].as_str().unwrap().to_owned();
        let at = Utc.with_ymd_and_hms(2024, 5, 6, 7, 8, 9).unwrap();
        let fm: clove_types::ItemFrontmatter = serde_json::from_value(json!({
            "schema": 1, "id": id, "title": title, "status": "open", "type": "bug",
            "priority": 1, "created": at, "updated": at, "closed": null,
            "assignee": null, "parent": null, "labels": [], "deps": [], "relates": [],
            "duplicates": [], "supersedes": [], "source_system": "tk",
            "external_ref": "tk-1"
        }))
        .unwrap();
        ItemPayload {
            frontmatter: fm,
            body: "body".to_owned(),
        }
    }

    #[test]
    fn a_created_item_keeps_its_own_fields_and_cannot_be_overwritten() {
        let (_dir, svc) = service();
        let it = item(&svc, "Imported");
        let id = it.frontmatter.id.to_string();
        svc.create_item(CreateItemParams { item: it.clone() })
            .unwrap();

        let got: ItemPayload =
            serde_json::from_value(svc.get(StoreIdParams { id: id.clone() }).unwrap()).unwrap();
        assert_eq!(got, it, "timestamps and external_ref are written as given");

        let again = svc.create_item(CreateItemParams { item: it }).unwrap_err();
        assert_eq!(again.code(), ("VALIDATION_ERROR", 4));
        assert!(again.to_string().contains("already exists"), "{again}");

        let scan: ScanPayload = serde_json::from_value(svc.scan().unwrap()).unwrap();
        assert_eq!(scan.items.len(), 1);
        assert!(scan.errors.is_empty());
    }

    #[test]
    fn create_item_validates_like_create() {
        let (_dir, svc) = service();
        let mut it = item(&svc, "x");
        it.frontmatter.title = String::new();
        let err = svc.create_item(CreateItemParams { item: it }).unwrap_err();
        assert_eq!(err.code().0, "VALIDATION_ERROR");
    }

    #[test]
    fn edits_links_deps_and_comments_go_through_the_write_path() {
        let (_dir, svc) = service();
        let a = item(&svc, "A");
        let b = item(&svc, "B");
        let (ida, idb) = (a.frontmatter.id.to_string(), b.frontmatter.id.to_string());
        svc.create_item(CreateItemParams { item: a }).unwrap();
        svc.create_item(CreateItemParams { item: b }).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 1, 2, 3, 4, 5).unwrap();

        let edited = svc
            .apply_edit(ApplyEditParams {
                id: ida.clone(),
                request: EditRequest {
                    title: Some("A2".to_owned()),
                    ..Default::default()
                },
                now,
            })
            .unwrap();
        assert_eq!(edited["title"], "A2");

        svc.dep_add(StoreDepParams {
            id: ida.clone(),
            dep: idb.clone(),
            now,
        })
        .unwrap();
        let cycle = svc
            .dep_add(StoreDepParams {
                id: idb.clone(),
                dep: ida.clone(),
                now,
            })
            .unwrap_err();
        assert_eq!(cycle.code(), ("CYCLE_DETECTED", 3));

        svc.link_external(LinkExternalParams {
            id: idb.clone(),
            source_system: "github".to_owned(),
            external_ref: "gh-7".to_owned(),
            now,
        })
        .unwrap();
        let got: ItemPayload =
            serde_json::from_value(svc.get(StoreIdParams { id: idb.clone() }).unwrap()).unwrap();
        assert_eq!(got.frontmatter.external_ref.as_deref(), Some("gh-7"));

        let when = Utc.with_ymd_and_hms(2023, 3, 3, 3, 3, 3).unwrap();
        svc.add_comment(AddCommentParams {
            id: idb.clone(),
            author: "octocat".to_owned(),
            body: "from GitHub".to_owned(),
            timestamp: when,
        })
        .unwrap();
        let comments: Vec<CommentPayload> =
            serde_json::from_value(svc.comments(StoreIdParams { id: idb }).unwrap()).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "octocat");
        assert_eq!(comments[0].timestamp, when);
    }
}
