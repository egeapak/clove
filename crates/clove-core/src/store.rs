//! The file store: CRUD over `.clove/issues/<id>.md` (DESIGN.md §2, §4).
//!
//! Files are the source of truth. Reads parse from disk; writes go through the
//! atomic rename in [`crate::write`]. Bulk scans skip symlinks (§12.3) and
//! report per-file parse failures without aborting the whole scan, so one
//! corrupt file never hides the rest of the repository.

use std::fs::OpenOptions;

use camino::{Utf8Path, Utf8PathBuf};
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use thiserror::Error;

use crate::error::CloveError;
use crate::fields;
use crate::id::{new_id, CloveId};
use crate::model::{
    truncate_to_seconds, Item, ItemFrontmatter, ItemStatus, ItemType, Priority,
    CURRENT_SCHEMA_VERSION,
};
use crate::parse::{parse_frontmatter_file, parse_item_file};
use crate::validate::validate_item;
use crate::write::write_item_file;

/// Parse in parallel only above this many files; below it, rayon's thread
/// wake-up cost outweighs the gain (DESIGN §13.2).
const PARALLEL_SCAN_THRESHOLD: usize = 500;

/// Everything needed to create a new item except the generated id and the
/// creation/update timestamps. Labels are expected to already be canonical
/// (see [`crate::normalize_label`]); the CLI normalizes at its boundary.
#[derive(Debug, Clone)]
pub struct NewItem {
    pub title: String,
    pub item_type: ItemType,
    pub priority: Priority,
    pub labels: Vec<String>,
    pub deps: Vec<CloveId>,
    pub parent: Option<CloveId>,
    pub assignee: Option<String>,
    pub body: String,
}

/// A per-file failure encountered while scanning the store.
#[derive(Debug, Error)]
pub enum ScanError {
    #[error("failed to parse `{path}`: {source}")]
    ParseFailed {
        path: Utf8PathBuf,
        #[source]
        source: CloveError,
    },
}

impl ScanError {
    /// The file that failed to parse.
    pub fn path(&self) -> &Utf8Path {
        match self {
            ScanError::ParseFailed { path, .. } => path,
        }
    }
}

/// The store-wide advisory write lock (`.clove/write.lock`), open but not yet
/// acquired. Obtain one with [`ItemStore::write_lock`], then hold the guard
/// from [`StoreWriteLock::lock`] across a read-modify-write window.
///
/// The lock lives in its own file with a stable inode: item files are replaced
/// by temp+rename on every write, so an advisory lock taken on an item file
/// itself cannot reliably serialize writers (a second writer that opened the
/// path before the first's rename would lock the unlinked old inode).
#[derive(Debug)]
pub struct StoreWriteLock {
    path: Utf8PathBuf,
    lock: fd_lock::RwLock<std::fs::File>,
}

impl StoreWriteLock {
    /// Block until the exclusive lock is acquired; it releases when the returned
    /// guard (or this struct) is dropped.
    pub fn lock(&mut self) -> Result<fd_lock::RwLockWriteGuard<'_, std::fs::File>, CloveError> {
        self.lock.write().map_err(|source| CloveError::Io {
            path: self.path.clone(),
            source,
        })
    }
}

/// A handle to a repository's item store.
#[derive(Debug, Clone)]
pub struct ItemStore {
    repo_root: Utf8PathBuf,
    issues_dir: Utf8PathBuf,
}

impl ItemStore {
    /// Open the store rooted at `repo_root` (the directory containing `.clove/`).
    pub fn new(repo_root: Utf8PathBuf) -> Self {
        let issues_dir = repo_root.join(".clove").join("issues");
        Self {
            repo_root,
            issues_dir,
        }
    }

    /// The repository root (the directory containing `.clove/`).
    pub fn repo_root(&self) -> &Utf8Path {
        &self.repo_root
    }

    /// The `.clove/issues/` directory.
    pub fn issues_dir(&self) -> &Utf8Path {
        &self.issues_dir
    }

    /// The item file path for `id`.
    pub fn path_for(&self, id: &CloveId) -> Utf8PathBuf {
        self.issues_dir.join(format!("{id}.md"))
    }

    /// The sibling per-item directory (holds the `comments/` subdirectory).
    pub fn item_dir(&self, id: &CloveId) -> Utf8PathBuf {
        self.issues_dir.join(id.as_str())
    }

    /// Create a new item, generating its id and writing its file.
    ///
    /// `now` is supplied by the caller (the CLI passes `Utc::now()`); it is
    /// truncated to whole seconds to match the canonical on-disk timestamp
    /// precision.
    ///
    /// Enforces the same field validations as the edit path (a title must be
    /// non-empty, an assignee must be non-blank) plus referential integrity for
    /// the graph edges the spec carries: every `deps` entry and the `parent`
    /// must name an existing item, matching `dep add`/`set_parent`.
    pub fn create(
        &self,
        prefix: &str,
        spec: NewItem,
        now: DateTime<Utc>,
    ) -> Result<Item, CloveError> {
        let title = fields::parse_title(&spec.title)?;
        let assignee = fields::parse_assignee(spec.assignee)?;

        // Everything from the referential-integrity checks through the write
        // runs under the store-wide write lock: a concurrent (locked) `delete`
        // could otherwise remove a dep/parent target between our existence
        // check and the write, leaving exactly the dangling reference the
        // check exists to prevent — and two concurrent creates could race id
        // generation.
        let mut lock = self.write_lock()?;
        let _guard = lock.lock()?;

        for dep in &spec.deps {
            if !self.exists(dep) {
                return Err(CloveError::NotFound {
                    id: dep.to_string(),
                });
            }
        }
        if let Some(parent) = &spec.parent {
            if !self.exists(parent) {
                return Err(CloveError::NotFound {
                    id: parent.to_string(),
                });
            }
        }

        // Match the serializer, which sorts + de-dupes lists at write time, so
        // the returned in-memory `Item` is identical to what a re-read parses
        // (the same guarantee `dep_add` maintains).
        let mut deps = spec.deps;
        deps.sort();
        deps.dedup();
        let mut labels = spec.labels;
        labels.sort();
        labels.dedup();

        let now = truncate_to_seconds(now);
        let id = new_id(prefix, &self.issues_dir)?;

        let frontmatter = ItemFrontmatter {
            schema: CURRENT_SCHEMA_VERSION,
            id: id.clone(),
            title,
            status: ItemStatus::Open,
            item_type: spec.item_type,
            priority: spec.priority,
            created: now,
            updated: now,
            closed: None,
            assignee,
            parent: spec.parent,
            labels,
            deps,
            relates: Vec::new(),
            duplicates: Vec::new(),
            supersedes: Vec::new(),
            source_system: None,
            external_ref: None,
        };
        let item = Item {
            frontmatter,
            body: spec.body,
        };

        ensure_valid(&item.frontmatter, &self.path_for(&id))?;
        write_item_file(&item, &self.path_for(&id))?;
        Ok(item)
    }

    /// Read and parse the item with the given id.
    pub fn get(&self, id: &CloveId) -> Result<Item, CloveError> {
        let path = self.path_for(id);
        if !path.exists() {
            return Err(CloveError::NotFound { id: id.to_string() });
        }
        parse_item_file(&path)
    }

    /// Whether an item file exists for `id`.
    pub fn exists(&self, id: &CloveId) -> bool {
        self.path_for(id).exists()
    }

    /// Persist `item`, stamping `updated = now` (truncated to seconds). Holds the
    /// store-wide write lock across validate + write.
    ///
    /// Note: `update` alone cannot cover the caller's earlier read. Callers that
    /// read-modify-write (`get` → mutate → persist) should use
    /// [`ItemStore::update_with`], which holds the lock across the *whole*
    /// window so a concurrent writer cannot interleave (DESIGN §4: lock before
    /// reading, hold through rename). Do not call `update` while already holding
    /// the [`StoreWriteLock`] — the advisory lock is not reentrant.
    pub fn update(&self, item: &Item, now: DateTime<Utc>) -> Result<Item, CloveError> {
        let mut lock = self.write_lock()?;
        let _guard = lock.lock()?;
        self.update_locked(item, now)
    }

    /// Read `id`, apply `mutate`, and persist — all under the store-wide write
    /// lock, so the read-modify-write is atomic with respect to every other
    /// writer that goes through the store (DESIGN §4).
    ///
    /// `mutate` may perform additional *reads* of the store (scans, existence
    /// checks) — they are covered by the same lock — but must not call
    /// [`ItemStore::update`]/[`ItemStore::update_with`], which would deadlock on
    /// the non-reentrant advisory lock.
    pub fn update_with<F>(
        &self,
        id: &CloveId,
        now: DateTime<Utc>,
        mutate: F,
    ) -> Result<Item, CloveError>
    where
        F: FnOnce(&mut Item) -> Result<(), CloveError>,
    {
        let mut lock = self.write_lock()?;
        let _guard = lock.lock()?;
        let mut item = self.get(id)?;
        mutate(&mut item)?;
        self.update_locked(&item, now)
    }

    /// Open (creating if needed) the store-wide advisory write lock
    /// (`.clove/write.lock`). The caller acquires it via
    /// [`StoreWriteLock::lock`] and holds the guard for the whole
    /// read-modify-write window.
    pub fn write_lock(&self) -> Result<StoreWriteLock, CloveError> {
        let path = self.repo_root.join(".clove").join("write.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.as_std_path())
            .map_err(|source| CloveError::Io {
                path: path.clone(),
                source,
            })?;
        Ok(StoreWriteLock {
            path,
            lock: fd_lock::RwLock::new(file),
        })
    }

    /// The unlocked stamp + validate + atomic-write tail shared by
    /// [`ItemStore::update`] and [`ItemStore::update_with`].
    fn update_locked(&self, item: &Item, now: DateTime<Utc>) -> Result<Item, CloveError> {
        let mut next = item.clone();
        next.frontmatter.updated = truncate_to_seconds(now);

        let path = self.path_for(&next.frontmatter.id);
        if !path.exists() {
            return Err(CloveError::NotFound {
                id: next.frontmatter.id.to_string(),
            });
        }
        ensure_valid(&next.frontmatter, &path)?;
        write_item_file(&next, &path)?;
        Ok(next)
    }

    /// Delete an item's file and its sibling comment directory (if any).
    ///
    /// Unless `force`, refuses with [`CloveError::HasDependents`] when other
    /// items list this id in their `deps`.
    pub fn delete(&self, id: &CloveId, force: bool) -> Result<(), CloveError> {
        // Held across the dependents check + removal so a concurrent locked
        // writer (e.g. `dep_add` re-verifying this id exists before writing
        // the edge) cannot interleave and end up referencing a deleted item.
        let mut lock = self.write_lock()?;
        let _guard = lock.lock()?;

        let path = self.path_for(id);
        if !path.exists() {
            return Err(CloveError::NotFound { id: id.to_string() });
        }

        if !force {
            let dependents = self.dependents_of(id);
            if !dependents.is_empty() {
                return Err(CloveError::HasDependents {
                    id: id.to_string(),
                    dependents: dependents.into_iter().map(|d| d.to_string()).collect(),
                });
            }
        }

        std::fs::remove_file(&path).map_err(|source| CloveError::Io {
            path: path.clone(),
            source,
        })?;

        let dir = self.item_dir(id);
        if dir.is_dir() {
            std::fs::remove_dir_all(&dir).map_err(|source| CloveError::Io {
                path: dir.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// Full scan: parse every item file, partitioning successes from per-file
    /// parse failures.
    pub fn scan(&self) -> Result<(Vec<Item>, Vec<ScanError>), CloveError> {
        let paths = self.item_file_paths()?;
        let parse = |path: &Utf8PathBuf| {
            parse_item_file(path).map_err(|source| ScanError::ParseFailed {
                path: path.clone(),
                source,
            })
        };
        let results: Vec<Result<Item, ScanError>> = if paths.len() > PARALLEL_SCAN_THRESHOLD {
            paths.par_iter().map(parse).collect()
        } else {
            paths.iter().map(parse).collect()
        };
        Ok(partition(results))
    }

    /// Successfully-parsed items only (parse failures are dropped). For surfacing
    /// failures (e.g. `clove doctor`), use [`ItemStore::scan`].
    pub fn list(&self) -> Result<Vec<Item>, CloveError> {
        Ok(self.scan()?.0)
    }

    /// Body-free scan: parse only frontmatter of every item file (the
    /// `ls`/`ready`/`blocked` fast path, DESIGN §13.3).
    pub fn scan_frontmatter(&self) -> Result<(Vec<ItemFrontmatter>, Vec<ScanError>), CloveError> {
        let paths = self.item_file_paths()?;
        let parse = |path: &Utf8PathBuf| {
            parse_frontmatter_file(path).map_err(|source| ScanError::ParseFailed {
                path: path.clone(),
                source,
            })
        };
        let results: Vec<Result<ItemFrontmatter, ScanError>> =
            if paths.len() > PARALLEL_SCAN_THRESHOLD {
                paths.par_iter().map(parse).collect()
            } else {
                paths.iter().map(parse).collect()
            };
        Ok(partition(results))
    }

    /// IDs of items whose `deps` list contains `id` (best-effort: unparseable
    /// files are skipped).
    fn dependents_of(&self, id: &CloveId) -> Vec<CloveId> {
        let Ok((frontmatters, _errors)) = self.scan_frontmatter() else {
            return Vec::new();
        };
        frontmatters
            .into_iter()
            .filter(|fm| fm.deps.contains(id))
            .map(|fm| fm.id)
            .collect()
    }

    /// Collect the candidate item file paths: real `.md` files only, skipping
    /// symlinks (§12.3), directories (comment dirs), temp files, and non-UTF-8
    /// names.
    fn item_file_paths(&self) -> Result<Vec<Utf8PathBuf>, CloveError> {
        let read_dir = std::fs::read_dir(&self.issues_dir).map_err(|source| CloveError::Io {
            path: self.issues_dir.clone(),
            source,
        })?;

        let mut paths = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|source| CloveError::Io {
                path: self.issues_dir.clone(),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| CloveError::Io {
                path: self.issues_dir.clone(),
                source,
            })?;
            // Never follow symlinks; skip directories (per-item comment dirs).
            if file_type.is_symlink() || !file_type.is_file() {
                continue;
            }
            let Ok(path) = Utf8PathBuf::from_path_buf(entry.path()) else {
                continue; // non-UTF-8 name cannot be a valid item id
            };
            let Some(name) = path.file_name() else {
                continue;
            };
            if !name.ends_with(".md") {
                continue;
            }
            paths.push(path);
        }
        Ok(paths)
    }
}

/// Return `Err(Invalid)` if the frontmatter has any validation errors.
fn ensure_valid(frontmatter: &ItemFrontmatter, path: &Utf8Path) -> Result<(), CloveError> {
    let errors = validate_item(frontmatter);
    if errors.is_empty() {
        return Ok(());
    }
    Err(CloveError::Invalid {
        path: path.to_owned(),
        count: errors.len(),
        summary: errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; "),
    })
}

/// Split a vector of results into (oks, errs).
fn partition<T, E>(results: Vec<Result<T, E>>) -> (Vec<T>, Vec<E>) {
    let mut oks = Vec::new();
    let mut errs = Vec::new();
    for result in results {
        match result {
            Ok(value) => oks.push(value),
            Err(error) => errs.push(error),
        }
    }
    (oks, errs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comments::add_comment;

    fn temp_store() -> (tempfile::TempDir, ItemStore) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8Path::from_path(tmp.path()).unwrap().to_owned();
        std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
        (tmp, ItemStore::new(root))
    }

    fn spec(title: &str) -> NewItem {
        NewItem {
            title: title.to_owned(),
            item_type: ItemType::Feature,
            priority: Priority::DEFAULT,
            labels: vec!["area:core".to_owned()],
            deps: Vec::new(),
            parent: None,
            assignee: None,
            body: "Body.\n".to_owned(),
        }
    }

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn create_get_update_list_roundtrip() {
        let (_tmp, store) = temp_store();
        let created = store
            .create("proj", spec("First"), ts("2026-06-02T10:00:00Z"))
            .unwrap();

        // get returns an identical item
        let fetched = store.get(&created.frontmatter.id).unwrap();
        assert_eq!(fetched, created);
        assert_eq!(fetched.frontmatter.title, "First");
        assert_eq!(fetched.frontmatter.status, ItemStatus::Open);

        // update bumps `updated` and persists field changes
        let mut edited = fetched.clone();
        edited.frontmatter.title = "First (edited)".to_owned();
        let updated = store.update(&edited, ts("2026-06-02T12:00:00Z")).unwrap();
        assert_eq!(updated.frontmatter.updated, ts("2026-06-02T12:00:00Z"));
        assert_ne!(updated.frontmatter.updated, created.frontmatter.updated);
        assert_eq!(updated.frontmatter.created, created.frontmatter.created);

        let reread = store.get(&created.frontmatter.id).unwrap();
        assert_eq!(reread.frontmatter.title, "First (edited)");
        assert_eq!(reread.frontmatter.updated, ts("2026-06-02T12:00:00Z"));

        // list returns the single item
        let all = store.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].frontmatter.id, created.frontmatter.id);
    }

    #[test]
    fn get_missing_is_not_found() {
        let (_tmp, store) = temp_store();
        let err = store
            .get(&CloveId::new("proj-00000000").unwrap())
            .unwrap_err();
        assert!(matches!(err, CloveError::NotFound { .. }));
    }

    #[test]
    fn scan_reports_parse_failures_without_aborting() {
        let (_tmp, store) = temp_store();
        store
            .create("proj", spec("Good"), ts("2026-06-02T10:00:00Z"))
            .unwrap();
        // A malformed file that still looks like an item file.
        std::fs::write(
            store.issues_dir().join("proj-BADBAD00.md"),
            "not valid frontmatter\n",
        )
        .unwrap();

        let (items, errors) = store.scan().unwrap();
        assert_eq!(items.len(), 1, "the good item still parses");
        assert_eq!(errors.len(), 1, "the malformed file is reported, not fatal");
        assert!(errors[0].path().as_str().ends_with("proj-BADBAD00.md"));
    }

    #[test]
    fn scan_skips_symlinks() {
        let (_tmp, store) = temp_store();
        let good = store
            .create("proj", spec("Real"), ts("2026-06-02T10:00:00Z"))
            .unwrap();

        // A symlink that points at the real item file must not be followed.
        #[cfg(unix)]
        {
            let link = store.issues_dir().join("proj-LINK0000.md");
            std::os::unix::fs::symlink(store.path_for(&good.frontmatter.id), &link).unwrap();
            let (items, errors) = store.scan().unwrap();
            assert_eq!(items.len(), 1, "symlink is not followed");
            assert!(errors.is_empty());
        }
        #[cfg(not(unix))]
        let _ = good;
    }

    #[test]
    fn delete_refuses_when_dependents_exist() {
        let (_tmp, store) = temp_store();
        let dep = store
            .create("proj", spec("Dependency"), ts("2026-06-02T10:00:00Z"))
            .unwrap();
        let mut dependent_spec = spec("Dependent");
        dependent_spec.deps = vec![dep.frontmatter.id.clone()];
        store
            .create("proj", dependent_spec, ts("2026-06-02T10:00:01Z"))
            .unwrap();

        let err = store.delete(&dep.frontmatter.id, false).unwrap_err();
        assert!(matches!(err, CloveError::HasDependents { .. }));

        // force deletes anyway
        store.delete(&dep.frontmatter.id, true).unwrap();
        assert!(!store.exists(&dep.frontmatter.id));
    }

    #[test]
    fn delete_removes_sibling_comment_directory() {
        let (_tmp, store) = temp_store();
        let item = store
            .create("proj", spec("Has comments"), ts("2026-06-02T10:00:00Z"))
            .unwrap();
        add_comment(
            store.issues_dir(),
            &item.frontmatter.id,
            "ege@example.com",
            "A comment.",
        )
        .unwrap();
        assert!(store.item_dir(&item.frontmatter.id).is_dir());

        store.delete(&item.frontmatter.id, false).unwrap();
        assert!(!store.path_for(&item.frontmatter.id).exists());
        assert!(!store.item_dir(&item.frontmatter.id).exists());
    }

    #[test]
    fn concurrent_creates_produce_distinct_valid_files() {
        use std::sync::Arc;
        use std::thread;

        let (_tmp, store) = temp_store();
        let store = Arc::new(store);
        let mut handles = Vec::new();
        for n in 0..10 {
            let store = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                store
                    .create(
                        "proj",
                        spec(&format!("Item {n}")),
                        ts("2026-06-02T10:00:00Z"),
                    )
                    .unwrap()
                    .frontmatter
                    .id
            }));
        }
        let ids: Vec<CloveId> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), 10, "ids must be distinct");

        let (items, errors) = store.scan().unwrap();
        assert!(errors.is_empty(), "all files valid: {errors:?}");
        assert_eq!(items.len(), 10);
    }
}
