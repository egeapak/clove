//! [`Store`] — every read and write the importers and the GitHub sync make, in
//! one interface with two implementations:
//!
//! - [`HostStore`] — the plugin path: calls the `clove` that runs the plugin
//!   over the host IPC channel (`store.*`, `PLUGIN_SYSTEM.md` §6.6), so all
//!   writes happen in the host. The shipped plugins (`clove-import-tk`,
//!   `clove-import-beads`, `clove-sync-github`) use this.
//! - [`ItemStore`] — direct access, for unit tests and in-process callers.
//!
//! Both implementations write through the same validated path:
//! `create_item` is `ItemStore::restore_item` (field checks like `create`, never
//! overwrites), edits are `apply_edit`, dependencies go through the
//! cycle-checked `ops`, comments through `add_comment_at`.

use std::cell::RefCell;

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use clove_core::{ItemStore, RestoreOutcome};
use clove_plugin::{HostClient, HostError};
use clove_types::tool::{CommentPayload, ItemPayload, ScanErrorPayload};
use clove_types::{CloveError, CloveId, EditRequest, Item, ItemFrontmatter};
use serde_json::Value;

use crate::ImportError;

/// The result of a full scan: the parsable items and the files that failed.
#[derive(Debug, Clone)]
pub struct Scan {
    pub items: Vec<Item>,
    pub errors: Vec<ScanErrorPayload>,
}

/// Store access for importers and sync. See the module docs.
pub trait Store {
    /// The repository root (sync bookkeeping lives under `.clove/sync/`).
    fn repo_root(&self) -> &Utf8Path;
    /// Every item, plus the files that failed to parse.
    fn scan(&self) -> Result<Scan, ImportError>;
    /// Every parsable item's frontmatter (parse failures are skipped).
    fn scan_frontmatter(&self) -> Result<Vec<ItemFrontmatter>, ImportError>;
    /// One whole item.
    fn get(&self, id: &CloveId) -> Result<Item, ImportError>;
    /// A fresh, unused id with the repository's prefix.
    fn new_id(&self) -> Result<CloveId, ImportError>;
    /// Write a new item exactly as given; an existing id is an error.
    fn create_item(&self, item: &Item) -> Result<(), ImportError>;
    /// The unified edit, stamped `now`; returns the edited item's JSON.
    fn apply_edit(
        &self,
        id: &CloveId,
        request: &EditRequest,
        now: DateTime<Utc>,
    ) -> Result<Value, ImportError>;
    /// Add a hard dependency (cycle-checked).
    fn dep_add(&self, id: &CloveId, dep: &CloveId, now: DateTime<Utc>) -> Result<(), ImportError>;
    /// Remove a hard dependency.
    fn dep_remove(
        &self,
        id: &CloveId,
        dep: &CloveId,
        now: DateTime<Utc>,
    ) -> Result<(), ImportError>;
    /// Set `source_system` + `external_ref`; returns the item's new `updated`.
    fn link_external(
        &self,
        id: &CloveId,
        source_system: &str,
        external_ref: &str,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, ImportError>;
    /// An item's comments, oldest first.
    fn comments(&self, id: &CloveId) -> Result<Vec<CommentPayload>, ImportError>;
    /// Append a comment with its original author and time.
    fn add_comment(
        &self,
        id: &CloveId,
        author: &str,
        body: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<(), ImportError>;
}

/// Direct access. `new_id` uses the prefix the store was opened with.
pub struct DirectStore {
    store: ItemStore,
    prefix: String,
}

impl DirectStore {
    pub fn new(store: ItemStore, prefix: impl Into<String>) -> Self {
        DirectStore {
            store,
            prefix: prefix.into(),
        }
    }

    /// The underlying store.
    pub fn inner(&self) -> &ItemStore {
        &self.store
    }
}

impl Store for DirectStore {
    fn repo_root(&self) -> &Utf8Path {
        self.store.repo_root()
    }

    fn scan(&self) -> Result<Scan, ImportError> {
        let (items, errors) = self.store.scan()?;
        Ok(Scan {
            items,
            errors: errors
                .iter()
                .map(|e| ScanErrorPayload {
                    path: e.path().to_string(),
                    message: e.to_string(),
                })
                .collect(),
        })
    }

    fn scan_frontmatter(&self) -> Result<Vec<ItemFrontmatter>, ImportError> {
        Ok(self.store.scan_frontmatter()?.0)
    }

    fn get(&self, id: &CloveId) -> Result<Item, ImportError> {
        Ok(self.store.get(id)?)
    }

    fn new_id(&self) -> Result<CloveId, ImportError> {
        Ok(clove_types::new_id(&self.prefix, self.store.issues_dir())?)
    }

    fn create_item(&self, item: &Item) -> Result<(), ImportError> {
        match self.store.restore_item(item, false)? {
            RestoreOutcome::Skipped => Err(CloveError::InvalidField {
                field: "id".to_owned(),
                reason: format!("item `{}` already exists", item.frontmatter.id),
            }
            .into()),
            _ => Ok(()),
        }
    }

    fn apply_edit(
        &self,
        id: &CloveId,
        request: &EditRequest,
        now: DateTime<Utc>,
    ) -> Result<Value, ImportError> {
        Ok(clove_core::apply_edit(&self.store, id, request, now)?)
    }

    fn dep_add(&self, id: &CloveId, dep: &CloveId, now: DateTime<Utc>) -> Result<(), ImportError> {
        clove_core::ops::dep_add(&self.store, id, dep, now)?;
        Ok(())
    }

    fn dep_remove(
        &self,
        id: &CloveId,
        dep: &CloveId,
        now: DateTime<Utc>,
    ) -> Result<(), ImportError> {
        clove_core::ops::dep_remove(&self.store, id, dep, now)?;
        Ok(())
    }

    fn link_external(
        &self,
        id: &CloveId,
        source_system: &str,
        external_ref: &str,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, ImportError> {
        let saved = self.store.update_with(id, now, |item| {
            item.frontmatter.source_system = Some(source_system.to_owned());
            item.frontmatter.external_ref = Some(external_ref.to_owned());
            Ok(())
        })?;
        Ok(saved.frontmatter.updated)
    }

    fn comments(&self, id: &CloveId) -> Result<Vec<CommentPayload>, ImportError> {
        Ok(
            clove_core::comments::list_comments(self.store.issues_dir(), id)?
                .into_iter()
                .map(|c| CommentPayload {
                    timestamp: c.timestamp,
                    author: c.author,
                    body: c.body,
                })
                .collect(),
        )
    }

    fn add_comment(
        &self,
        id: &CloveId,
        author: &str,
        body: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<(), ImportError> {
        clove_core::comments::add_comment_at(self.store.issues_dir(), id, author, body, timestamp)?;
        Ok(())
    }
}

/// The plugin path: every call goes to the host over the IPC channel.
pub struct HostStore {
    client: RefCell<HostClient>,
    repo_root: Utf8PathBuf,
}

impl HostStore {
    /// Wrap an open host connection. `repo_root` is the plugin context's root
    /// (`$CLOVE_ROOT`), used only to place sync bookkeeping files.
    pub fn new(client: HostClient, repo_root: Utf8PathBuf) -> Self {
        HostStore {
            client: RefCell::new(client),
            repo_root,
        }
    }

    /// Open the host channel of the `clove` that runs this plugin. Fails with
    /// the SDK's "run this plugin through `clove`" error when there is none.
    pub fn connect(cx: &clove_plugin::PluginContext) -> Result<Self, CloveError> {
        Ok(HostStore::new(cx.host()?, cx.root.clone()))
    }

    fn with<T>(
        &self,
        call: impl FnOnce(&mut HostClient) -> Result<T, HostError>,
    ) -> Result<T, ImportError> {
        call(&mut self.client.borrow_mut()).map_err(|e| ImportError::Core(e.into()))
    }
}

impl Store for HostStore {
    fn repo_root(&self) -> &Utf8Path {
        &self.repo_root
    }

    fn scan(&self) -> Result<Scan, ImportError> {
        let scan = self.with(|c| c.store_scan())?;
        Ok(Scan {
            items: scan.items.into_iter().map(Item::from).collect(),
            errors: scan.errors,
        })
    }

    fn scan_frontmatter(&self) -> Result<Vec<ItemFrontmatter>, ImportError> {
        Ok(self.with(|c| c.store_scan_frontmatter())?.frontmatters)
    }

    fn get(&self, id: &CloveId) -> Result<Item, ImportError> {
        Ok(self.with(|c| c.store_get(id.as_str()))?.into())
    }

    fn new_id(&self) -> Result<CloveId, ImportError> {
        let raw = self.with(|c| c.store_new_id())?;
        Ok(CloveId::new(&raw)?)
    }

    fn create_item(&self, item: &Item) -> Result<(), ImportError> {
        let payload = ItemPayload::from(item.clone());
        self.with(|c| c.store_create_item(&payload))
    }

    fn apply_edit(
        &self,
        id: &CloveId,
        request: &EditRequest,
        now: DateTime<Utc>,
    ) -> Result<Value, ImportError> {
        self.with(|c| c.store_apply_edit(id.as_str(), request, now))
    }

    fn dep_add(&self, id: &CloveId, dep: &CloveId, now: DateTime<Utc>) -> Result<(), ImportError> {
        self.with(|c| c.store_dep_add(id.as_str(), dep.as_str(), now))
            .map(drop)
    }

    fn dep_remove(
        &self,
        id: &CloveId,
        dep: &CloveId,
        now: DateTime<Utc>,
    ) -> Result<(), ImportError> {
        self.with(|c| c.store_dep_remove(id.as_str(), dep.as_str(), now))
            .map(drop)
    }

    fn link_external(
        &self,
        id: &CloveId,
        source_system: &str,
        external_ref: &str,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, ImportError> {
        self.with(|c| c.store_link_external(id.as_str(), source_system, external_ref, now))
    }

    fn comments(&self, id: &CloveId) -> Result<Vec<CommentPayload>, ImportError> {
        self.with(|c| c.store_comments(id.as_str()))
    }

    fn add_comment(
        &self,
        id: &CloveId,
        author: &str,
        body: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<(), ImportError> {
        self.with(|c| c.store_add_comment(id.as_str(), author, body, timestamp))
    }
}
