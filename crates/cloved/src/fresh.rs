//! What the daemon knows about how far its index is behind the files.
//!
//! The file watch reports every change under `issues/` the moment it happens.
//! [`Freshness`] collects those reports, so a read can bring the index up to
//! date with a check of **only the files that changed** instead of a pass over
//! the whole store, and so a watcher batch does the same (an editor's in-place
//! save changes neither the directory mtime nor the file count, so only the
//! named file's own stat shows it).
//!
//! Two things can make the reports incomplete, and both are detected:
//!
//! - **The watch is lost.** `issues/` was removed or replaced (a `git checkout`
//!   across a commit without the store, `git stash -u`): the OS watch was on
//!   the old directory and reports nothing more. [`Freshness::sync`] answers
//!   [`Sync::Lost`], and the watcher task ends, which unloads the project. The
//!   next call loads it again with a fresh watch.
//! - **Events were dropped** (a queue overflow), or the directory changed with
//!   no report at all. The next sync is a full per-file pass.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use camino::Utf8PathBuf;
use clove_index::Index;
use clove_types::CloveId;

/// The identity of a file or directory: stable across renames of other things,
/// different once the path names a new object. `None` where the platform has
/// no cheap one (then the path is assumed to name the same object).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId(u64, u64);

/// The identity of whatever `path` names now, or `None` if it cannot be read.
pub fn file_id(path: &Path) -> Option<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        Some(FileId(meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Changes the watch has reported and no sync has consumed yet.
#[derive(Default)]
struct Pending {
    ids: HashSet<CloveId>,
    /// Anything at all was reported under `issues/` (an item file or not).
    activity: bool,
}

/// How a [`Freshness::sync`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sync {
    /// The index matches the files. `changed` says whether a row moved.
    Fresh { changed: bool },
    /// More items changed than the caller allowed; nothing was applied.
    Behind,
    /// The watch no longer covers `issues/`; the index cannot be trusted.
    Lost,
    /// The check or the apply failed; nothing was consumed.
    Failed,
}

/// One project's freshness bookkeeping, shared by the watch callback, the
/// watcher task, and the request handlers.
pub struct Freshness {
    issues_dir: Utf8PathBuf,
    pending: Mutex<Pending>,
    /// The directory mtime the index was last brought up to. A different
    /// value with nothing pending means a change the watch did not report.
    seen_dir_mtime: Mutex<Option<SystemTime>>,
    /// The directory the watch was armed on.
    root: Mutex<Option<FileId>>,
    lost: AtomicBool,
    /// The next sync must be a full per-file pass.
    resweep: AtomicBool,
    /// Wakes the watcher task: the watch is lost, or a sync is owed.
    wake: tokio::sync::Notify,
}

impl Freshness {
    pub fn new(issues_dir: Utf8PathBuf) -> Freshness {
        Freshness {
            issues_dir,
            pending: Mutex::new(Pending::default()),
            seen_dir_mtime: Mutex::new(None),
            root: Mutex::new(None),
            lost: AtomicBool::new(false),
            resweep: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
        }
    }

    /// Record that the watch now covers the current `issues/`.
    pub fn armed(&self) {
        *lock(&self.root) = file_id(self.issues_dir.as_std_path());
    }

    /// One report from the watch. Runs on the watch's own thread.
    pub fn note_event(&self, event: &notify::Event) {
        use notify::event::{EventKind, ModifyKind};
        if event.need_rescan() {
            self.owe_resweep();
        }
        let gone = matches!(
            event.kind,
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        let mut pending = lock(&self.pending);
        pending.activity = true;
        for path in &event.paths {
            if path == self.issues_dir.as_std_path() {
                if gone {
                    drop(pending);
                    self.mark_lost();
                    return;
                }
                continue;
            }
            if let Some(id) = self.item_id(path) {
                pending.ids.insert(id);
            }
        }
    }

    /// The watch reported an error: some reports may be missing.
    pub fn note_error(&self) {
        self.owe_resweep();
    }

    /// The id of the item file `path` names, if it is one: `issues/<id>.md`.
    fn item_id(&self, path: &Path) -> Option<CloveId> {
        if path.parent() != Some(self.issues_dir.as_std_path()) {
            return None;
        }
        let stem = path.file_name()?.to_str()?.strip_suffix(".md")?;
        CloveId::new(stem).ok()
    }

    fn owe_resweep(&self) {
        self.resweep.store(true, Ordering::Relaxed);
        self.wake.notify_one();
    }

    fn mark_lost(&self) {
        self.lost.store(true, Ordering::Relaxed);
        self.wake.notify_one();
    }

    /// Whether the watch is known to be gone.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    /// Resolves when the watcher task has something to do besides a batch.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }

    /// Check that `issues/` is still the directory the watch was armed on,
    /// and mark the watch lost if not. Returns the directory's mtime.
    fn stat_root(&self) -> Option<SystemTime> {
        let meta = std::fs::metadata(self.issues_dir.as_std_path())
            .ok()
            .filter(std::fs::Metadata::is_dir);
        let armed_on = *lock(&self.root);
        let same = match (&meta, armed_on) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(_), Some(id)) => file_id(self.issues_dir.as_std_path()) == Some(id),
        };
        if !same {
            self.mark_lost();
            return None;
        }
        meta.and_then(|m| m.modified().ok())
    }

    /// Whether the watch still covers `issues/`. Marks it lost if not.
    pub fn root_intact(&self) -> bool {
        !self.is_lost() && self.stat_root().is_some()
    }

    /// Bring `index` up to date with the files.
    ///
    /// In the steady state this is one `stat` of the directory. With reports
    /// pending it checks exactly those files. With a directory change that no
    /// report explains, or reports known to be missing, it runs the full
    /// per-file pass. `limit` caps how many changed items one call may apply
    /// (a request handler passes one, so a large catch-up is left to the
    /// watcher task and the caller is told [`Sync::Behind`]).
    pub fn sync(&self, index: &mut Index, limit: Option<usize>) -> Sync {
        if self.is_lost() {
            return Sync::Lost;
        }
        let Some(dir_mtime) = self.stat_root() else {
            return Sync::Lost;
        };
        let taken = std::mem::take(&mut *lock(&self.pending));
        // A directory that moved since the last sync gets the full pass, also
        // when reports are pending: reports can arrive late and in parts (the
        // event for a writer's temporary file before the event for its rename,
        // most visibly on macOS), so pending reports do not show that every
        // change behind the new mtime has been reported.
        let unexplained = *lock(&self.seen_dir_mtime) != Some(dir_mtime);
        let full = unexplained || self.resweep.swap(false, Ordering::Relaxed);

        let report = if full {
            index.check_staleness(&self.issues_dir)
        } else if taken.ids.is_empty() {
            *lock(&self.seen_dir_mtime) = Some(dir_mtime);
            return Sync::Fresh { changed: false };
        } else {
            let ids: Vec<CloveId> = taken.ids.iter().cloned().collect();
            index.check_staleness_of(&self.issues_dir, &ids)
        };
        let outcome = match report {
            Err(_) => Sync::Failed,
            Ok(report) if limit.is_some_and(|max| report.change_count() > max) => Sync::Behind,
            Ok(report) if !report.needs_apply() => Sync::Fresh { changed: false },
            Ok(report) => match index.apply_staleness(&report, &self.issues_dir) {
                Ok(()) => Sync::Fresh {
                    changed: !report.is_clean(),
                },
                Err(_) => Sync::Failed,
            },
        };
        match outcome {
            Sync::Fresh { .. } => *lock(&self.seen_dir_mtime) = Some(dir_mtime),
            _ => {
                // Nothing was consumed: put the reports back and leave the
                // work to the watcher task, which applies without a limit and
                // retries a failure.
                let mut pending = lock(&self.pending);
                pending.ids.extend(taken.ids);
                pending.activity |= taken.activity;
                drop(pending);
                if full {
                    self.resweep.store(true, Ordering::Relaxed);
                }
                self.wake.notify_one();
            }
        }
        outcome
    }

    /// The first sync of a load, before any watch: the directory-level check
    /// (a full pass only if the directory moved since the index was written).
    pub fn startup_sweep(&self, index: &mut Index) -> bool {
        let dir_mtime = std::fs::metadata(self.issues_dir.as_std_path())
            .and_then(|m| m.modified())
            .ok();
        let mut applied = false;
        if let Ok(report) = index.check_staleness_fast(&self.issues_dir) {
            if report.needs_apply() && index.apply_staleness(&report, &self.issues_dir).is_ok() {
                applied = !report.is_clean();
            }
        }
        *lock(&self.seen_dir_mtime) = dir_mtime;
        applied
    }

    /// The daemon itself just wrote `id`'s file: index it now, without waiting
    /// for the watch to report the daemon's own write.
    pub fn wrote(&self, index: &mut Index, id: &CloveId) {
        let dir_mtime = std::fs::metadata(self.issues_dir.as_std_path())
            .and_then(|m| m.modified())
            .ok();
        let applied = index
            .check_staleness_of(&self.issues_dir, std::slice::from_ref(id))
            .and_then(|report| index.apply_staleness(&report, &self.issues_dir));
        match applied {
            // The directory moved because of this write; any other writer's
            // change is still reported by the watch.
            Ok(()) => *lock(&self.seen_dir_mtime) = dir_mtime,
            Err(_) => {
                lock(&self.pending).ids.insert(id.clone());
                self.wake.notify_one();
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{DataChange, EventKind, ModifyKind, RemoveKind};

    fn item_md(id: &str, title: &str) -> String {
        format!(
            "---\nschema: 1\nid: {id}\ntitle: {title}\nstatus: open\ntype: feature\n\
             priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\nbody\n"
        )
    }

    fn id(n: usize) -> CloveId {
        CloveId::new(&format!("proj-{n:08}")).unwrap()
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        issues: Utf8PathBuf,
        index: Index,
        fresh: Freshness,
    }

    impl Fixture {
        fn write(&self, n: usize, title: &str) {
            let id = id(n);
            std::fs::write(
                self.issues.join(format!("{id}.md")),
                item_md(id.as_str(), title),
            )
            .unwrap();
        }

        /// The watch reports a change of item `n`'s file.
        fn report(&self, n: usize) {
            let path = self.issues.join(format!("{}.md", id(n)));
            let event = notify::Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
                .add_path(path.into_std_path_buf());
            self.fresh.note_event(&event);
        }

        fn title(&self, n: usize) -> Option<String> {
            let rows = self
                .index
                .query_list(&clove_index::Filter::default())
                .unwrap();
            rows.into_iter()
                .find(|row| row.id.as_str() == id(n).as_str())
                .map(|row| row.title)
        }
    }

    /// A store of `n` items, indexed, swept, and "armed".
    fn fixture(n: usize) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let issues = root.join("issues");
        std::fs::create_dir_all(&issues).unwrap();
        for i in 0..n {
            let id = id(i);
            std::fs::write(
                issues.join(format!("{id}.md")),
                item_md(id.as_str(), &format!("Item {i}")),
            )
            .unwrap();
        }
        let db = root.join("index.db");
        clove_index::reindex(&issues, &db).unwrap();
        let mut index = Index::open(&db).unwrap();
        let fresh = Freshness::new(issues.clone());
        fresh.startup_sweep(&mut index);
        fresh.armed();
        Fixture {
            _tmp: tmp,
            issues,
            index,
            fresh,
        }
    }

    #[test]
    fn a_reported_in_place_save_is_applied() {
        // An editor's in-place save: same directory mtime, same file count.
        let mut fx = fixture(3);
        fx.write(1, "Saved in place");
        fx.report(1);
        assert_eq!(
            fx.fresh.sync(&mut fx.index, Some(20)),
            Sync::Fresh { changed: true }
        );
        assert_eq!(fx.title(1).as_deref(), Some("Saved in place"));
    }

    #[test]
    fn a_sync_checks_only_the_reported_files() {
        // Two files rewritten in place, one reported: only that one is read.
        // The cost of a sync follows the number of reports, not the store.
        let mut fx = fixture(3);
        fx.write(1, "Reported");
        fx.write(2, "Not reported");
        fx.report(1);
        fx.fresh.sync(&mut fx.index, None);
        assert_eq!(fx.title(1).as_deref(), Some("Reported"));
        assert_eq!(fx.title(2).as_deref(), Some("Item 2"));
        // With nothing reported, a sync changes nothing.
        assert_eq!(
            fx.fresh.sync(&mut fx.index, None),
            Sync::Fresh { changed: false }
        );
    }

    #[test]
    fn a_change_over_the_limit_is_refused_and_kept_for_the_watcher() {
        let mut fx = fixture(2);
        for n in 10..40 {
            fx.write(n, "new");
            fx.report(n);
        }
        // A request handler's sync: too many, so nothing is applied…
        assert_eq!(fx.fresh.sync(&mut fx.index, Some(20)), Sync::Behind);
        assert_eq!(fx.index.item_count().unwrap(), 2);
        // …and nothing is forgotten: the watcher's sync applies all of it.
        assert_eq!(
            fx.fresh.sync(&mut fx.index, None),
            Sync::Fresh { changed: true }
        );
        assert_eq!(fx.index.item_count().unwrap(), 32);
    }

    #[test]
    fn a_directory_change_with_no_report_gets_the_full_pass() {
        let mut fx = fixture(2);
        // A new file moves the directory mtime; pretend its report was lost.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fx.write(7, "unreported");
        assert_eq!(
            fx.fresh.sync(&mut fx.index, None),
            Sync::Fresh { changed: true }
        );
        assert_eq!(fx.title(7).as_deref(), Some("unreported"));
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_issues_directory_loses_the_watch() {
        // `git checkout` across a commit without the store: `issues/` is a new
        // directory, and the OS watch was on the old one.
        let mut fx = fixture(2);
        let moved = fx.issues.with_file_name("issues.old");
        std::fs::rename(&fx.issues, &moved).unwrap();
        std::fs::create_dir(&fx.issues).unwrap();
        fx.write(5, "in the new directory");

        assert!(!fx.fresh.root_intact());
        assert_eq!(fx.fresh.sync(&mut fx.index, None), Sync::Lost);
        assert!(fx.fresh.is_lost());
    }

    #[test]
    fn a_reported_removal_of_the_root_loses_the_watch() {
        let fx = fixture(1);
        let event = notify::Event::new(EventKind::Remove(RemoveKind::Folder))
            .add_path(fx.issues.clone().into_std_path_buf());
        fx.fresh.note_event(&event);
        assert!(fx.fresh.is_lost());
    }

    #[test]
    fn the_daemons_own_write_is_indexed_without_a_report() {
        let mut fx = fixture(2);
        fx.write(9, "written by the daemon");
        fx.fresh.wrote(&mut fx.index, &id(9));
        assert_eq!(fx.title(9).as_deref(), Some("written by the daemon"));
        // The directory moved because of that write: no full pass is owed.
        fx.write(1, "rewritten, not reported");
        fx.fresh.sync(&mut fx.index, None);
        assert_eq!(fx.title(1).as_deref(), Some("Item 1"));
    }
    #[test]
    fn a_new_file_is_indexed_when_only_part_of_its_reports_arrived() {
        // A writer creates a temporary file and renames it to the item file.
        // The watch has reported the temporary file and not yet the rename.
        let mut f = fixture(2);
        std::thread::sleep(std::time::Duration::from_millis(20));
        f.write(7, "New item");
        let event = notify::Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
            .add_path(f.issues.join(".tmpAbC123").into_std_path_buf());
        f.fresh.note_event(&event);

        assert_eq!(
            f.fresh.sync(&mut f.index, None),
            Sync::Fresh { changed: true }
        );
        assert_eq!(f.title(7).as_deref(), Some("New item"));
    }
}
