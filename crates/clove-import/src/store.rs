//! [`Store`] — every read and write the importers and the GitHub sync make, in
//! one interface with two implementations:
//!
//! - [`HostStore`] — the plugin path: calls the `clove` that runs the plugin
//!   over the host IPC channel (`store.*`, `PLUGIN_SYSTEM.md` §6.6), so all
//!   writes happen in the host. The shipped plugins (`clove-import-tk`,
//!   `clove-import-beads`, `clove-sync-github`) use this.
//! - [`ItemStore`] — direct access, for unit tests and in-process callers.
//!
//! Both implementations write through the same validated ops in
//! `clove_core::ops` (`create_item`, `apply_edit`, the cycle-checked `dep_*`,
//! `link_external`, `comment_at`). The host runs them in the `cloved` daemon
//! when one is running, and in its own process otherwise.

use std::cell::RefCell;

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use clove_core::{ops, ItemStore};
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
    /// The unified edit; returns the edited item's JSON. The writer stamps
    /// `updated`.
    fn apply_edit(&self, id: &CloveId, request: &EditRequest) -> Result<Value, ImportError>;
    /// Add a hard dependency (cycle-checked).
    fn dep_add(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError>;
    /// Remove a hard dependency.
    fn dep_remove(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError>;
    /// Set `source_system` + `external_ref`; returns the item's new `updated`.
    fn link_external(
        &self,
        id: &CloveId,
        source_system: &str,
        external_ref: &str,
    ) -> Result<DateTime<Utc>, ImportError>;
    /// An item's comments, oldest first.
    fn comments(&self, id: &CloveId) -> Result<Vec<CommentPayload>, ImportError>;
    /// Append a comment with its original author and time; returns the new
    /// comment's key ([`CommentPayload::key`]).
    fn add_comment(
        &self,
        id: &CloveId,
        author: &str,
        body: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<String, ImportError>;
    /// Replace the body of the comment `key`, keeping its author and time.
    fn update_comment(&self, id: &CloveId, key: &str, body: &str) -> Result<(), ImportError>;
    /// Remove the comment `key`.
    fn delete_comment(&self, id: &CloveId, key: &str) -> Result<(), ImportError>;
}

/// The key (file name) in an `ops::comment*` result's repo-relative `path`.
fn comment_key(saved: &Value) -> String {
    saved["path"]
        .as_str()
        .and_then(|path| path.rsplit(['/', '\\']).next())
        .unwrap_or_default()
        .to_owned()
}

/// A [`Store`] that scans once until the next write.
///
/// One sync run reads the whole store several times: the planner's input, the
/// `external_ref` index of each comment plan, and the assignee baseline. Each
/// read is a full file scan (over the host IPC channel, for a plugin). This
/// wrapper keeps the last [`Store::scan`] and answers [`Store::scan`] and
/// [`Store::scan_frontmatter`] from it. Every write method drops the copy, so
/// a read after a write sees the write. Reads of one item ([`Store::get`],
/// [`Store::comments`]) are not cached.
pub struct CachedStore<'a> {
    inner: &'a dyn Store,
    scan: RefCell<Option<Scan>>,
}

impl<'a> CachedStore<'a> {
    pub fn new(inner: &'a dyn Store) -> Self {
        CachedStore {
            inner,
            scan: RefCell::new(None),
        }
    }

    fn cached(&self) -> Result<Scan, ImportError> {
        if let Some(scan) = self.scan.borrow().as_ref() {
            return Ok(scan.clone());
        }
        let scan = self.inner.scan()?;
        *self.scan.borrow_mut() = Some(scan.clone());
        Ok(scan)
    }

    /// Run a write and drop the cached scan, whether the write failed or not
    /// (a failed write can still have changed a file).
    fn write<T>(&self, op: impl FnOnce(&dyn Store) -> T) -> T {
        let out = op(self.inner);
        self.scan.borrow_mut().take();
        out
    }
}

impl Store for CachedStore<'_> {
    fn repo_root(&self) -> &Utf8Path {
        self.inner.repo_root()
    }

    fn scan(&self) -> Result<Scan, ImportError> {
        self.cached()
    }

    fn scan_frontmatter(&self) -> Result<Vec<ItemFrontmatter>, ImportError> {
        Ok(self
            .cached()?
            .items
            .into_iter()
            .map(|item| item.frontmatter)
            .collect())
    }

    fn get(&self, id: &CloveId) -> Result<Item, ImportError> {
        self.inner.get(id)
    }

    fn new_id(&self) -> Result<CloveId, ImportError> {
        self.inner.new_id()
    }

    fn create_item(&self, item: &Item) -> Result<(), ImportError> {
        self.write(|s| s.create_item(item))
    }

    fn apply_edit(&self, id: &CloveId, request: &EditRequest) -> Result<Value, ImportError> {
        self.write(|s| s.apply_edit(id, request))
    }

    fn dep_add(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError> {
        self.write(|s| s.dep_add(id, dep))
    }

    fn dep_remove(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError> {
        self.write(|s| s.dep_remove(id, dep))
    }

    fn link_external(
        &self,
        id: &CloveId,
        source_system: &str,
        external_ref: &str,
    ) -> Result<DateTime<Utc>, ImportError> {
        self.write(|s| s.link_external(id, source_system, external_ref))
    }

    fn comments(&self, id: &CloveId) -> Result<Vec<CommentPayload>, ImportError> {
        self.inner.comments(id)
    }

    fn add_comment(
        &self,
        id: &CloveId,
        author: &str,
        body: &str,
        timestamp: DateTime<Utc>,
    ) -> Result<String, ImportError> {
        self.write(|s| s.add_comment(id, author, body, timestamp))
    }

    fn update_comment(&self, id: &CloveId, key: &str, body: &str) -> Result<(), ImportError> {
        self.write(|s| s.update_comment(id, key, body))
    }

    fn delete_comment(&self, id: &CloveId, key: &str) -> Result<(), ImportError> {
        self.write(|s| s.delete_comment(id, key))
    }
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
        ops::create_item(&self.store, item)?;
        Ok(())
    }

    fn apply_edit(&self, id: &CloveId, request: &EditRequest) -> Result<Value, ImportError> {
        Ok(clove_core::apply_edit(
            &self.store,
            id,
            request,
            Utc::now(),
        )?)
    }

    fn dep_add(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError> {
        ops::dep_add(&self.store, id, dep, Utc::now())?;
        Ok(())
    }

    fn dep_remove(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError> {
        ops::dep_remove(&self.store, id, dep, Utc::now())?;
        Ok(())
    }

    fn link_external(
        &self,
        id: &CloveId,
        source_system: &str,
        external_ref: &str,
    ) -> Result<DateTime<Utc>, ImportError> {
        let saved = ops::link_external(&self.store, id, source_system, external_ref, Utc::now())?;
        serde_json::from_value(saved["updated"].clone()).map_err(|e| ImportError::Record {
            message: format!("link_external returned no `updated` stamp: {e}"),
        })
    }

    fn comments(&self, id: &CloveId) -> Result<Vec<CommentPayload>, ImportError> {
        Ok(
            clove_core::comments::list_comments(self.store.issues_dir(), id)?
                .into_iter()
                .map(|c| CommentPayload {
                    key: c.name,
                    timestamp: c.timestamp,
                    author: c.author,
                    body: c.body,
                    modified: c.modified,
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
    ) -> Result<String, ImportError> {
        let saved = ops::comment_at(&self.store, id, author, body, timestamp)?;
        Ok(comment_key(&saved))
    }

    fn update_comment(&self, id: &CloveId, key: &str, body: &str) -> Result<(), ImportError> {
        ops::comment_update(&self.store, id, key, body)?;
        Ok(())
    }

    fn delete_comment(&self, id: &CloveId, key: &str) -> Result<(), ImportError> {
        ops::comment_delete(&self.store, id, key)?;
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

    fn apply_edit(&self, id: &CloveId, request: &EditRequest) -> Result<Value, ImportError> {
        self.with(|c| c.store_apply_edit(id.as_str(), request))
    }

    fn dep_add(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError> {
        self.with(|c| c.store_dep_add(id.as_str(), dep.as_str()))
            .map(drop)
    }

    fn dep_remove(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError> {
        self.with(|c| c.store_dep_remove(id.as_str(), dep.as_str()))
            .map(drop)
    }

    fn link_external(
        &self,
        id: &CloveId,
        source_system: &str,
        external_ref: &str,
    ) -> Result<DateTime<Utc>, ImportError> {
        self.with(|c| c.store_link_external(id.as_str(), source_system, external_ref))
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
    ) -> Result<String, ImportError> {
        self.with(|c| c.store_add_comment(id.as_str(), author, body, timestamp))
    }

    fn update_comment(&self, id: &CloveId, key: &str, body: &str) -> Result<(), ImportError> {
        self.with(|c| c.store_update_comment(id.as_str(), key, body))
    }

    fn delete_comment(&self, id: &CloveId, key: &str) -> Result<(), ImportError> {
        self.with(|c| c.store_delete_comment(id.as_str(), key))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A [`DirectStore`] that counts the full scans it answers.
    struct Counting {
        inner: DirectStore,
        scans: Cell<usize>,
    }

    impl Store for Counting {
        fn repo_root(&self) -> &Utf8Path {
            self.inner.repo_root()
        }
        fn scan(&self) -> Result<Scan, ImportError> {
            self.scans.set(self.scans.get() + 1);
            self.inner.scan()
        }
        fn scan_frontmatter(&self) -> Result<Vec<ItemFrontmatter>, ImportError> {
            self.scans.set(self.scans.get() + 1);
            self.inner.scan_frontmatter()
        }
        fn get(&self, id: &CloveId) -> Result<Item, ImportError> {
            self.inner.get(id)
        }
        fn new_id(&self) -> Result<CloveId, ImportError> {
            self.inner.new_id()
        }
        fn create_item(&self, item: &Item) -> Result<(), ImportError> {
            self.inner.create_item(item)
        }
        fn apply_edit(&self, id: &CloveId, request: &EditRequest) -> Result<Value, ImportError> {
            self.inner.apply_edit(id, request)
        }
        fn dep_add(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError> {
            self.inner.dep_add(id, dep)
        }
        fn dep_remove(&self, id: &CloveId, dep: &CloveId) -> Result<(), ImportError> {
            self.inner.dep_remove(id, dep)
        }
        fn link_external(
            &self,
            id: &CloveId,
            source_system: &str,
            external_ref: &str,
        ) -> Result<DateTime<Utc>, ImportError> {
            self.inner.link_external(id, source_system, external_ref)
        }
        fn comments(&self, id: &CloveId) -> Result<Vec<CommentPayload>, ImportError> {
            self.inner.comments(id)
        }
        fn add_comment(
            &self,
            id: &CloveId,
            author: &str,
            body: &str,
            timestamp: DateTime<Utc>,
        ) -> Result<String, ImportError> {
            self.inner.add_comment(id, author, body, timestamp)
        }
        fn update_comment(&self, id: &CloveId, key: &str, body: &str) -> Result<(), ImportError> {
            self.inner.update_comment(id, key, body)
        }
        fn delete_comment(&self, id: &CloveId, key: &str) -> Result<(), ImportError> {
            self.inner.delete_comment(id, key)
        }
    }

    #[test]
    fn cached_store_scans_once_until_a_write() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap().to_owned();
        std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
        let counting = Counting {
            inner: DirectStore::new(ItemStore::new(root), "proj"),
            scans: Cell::new(0),
        };
        let created = counting
            .inner
            .inner()
            .create(
                "proj",
                clove_core::NewItem {
                    title: "First".to_owned(),
                    item_type: clove_types::ItemType::default(),
                    priority: clove_types::Priority::DEFAULT,
                    labels: vec![],
                    deps: vec![],
                    parent: None,
                    assignee: None,
                    body: "Body.".to_owned(),
                },
                Utc::now(),
            )
            .unwrap();
        let id = created.frontmatter.id;
        let cached = CachedStore::new(&counting);

        // Reads share one scan, frontmatter-only reads included.
        assert_eq!(cached.scan().unwrap().items.len(), 1);
        assert_eq!(cached.scan_frontmatter().unwrap()[0].title, "First");
        assert_eq!(cached.scan().unwrap().items.len(), 1);
        assert_eq!(counting.scans.get(), 1);

        // A write drops the scan, so the next read sees the write.
        cached
            .link_external(&id, "github", "gh-7")
            .expect("link the item");
        let frontmatters = cached.scan_frontmatter().unwrap();
        assert_eq!(frontmatters[0].external_ref.as_deref(), Some("gh-7"));
        assert_eq!(counting.scans.get(), 2);

        // A failed write drops it too.
        let missing = CloveId::new("proj-ZZZZZZZZ").unwrap();
        assert!(cached.link_external(&missing, "github", "gh-8").is_err());
        cached.scan().unwrap();
        assert_eq!(counting.scans.get(), 3);
    }
}
