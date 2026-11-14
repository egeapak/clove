//! The daemon's read connections to a project's `index.db`.
//!
//! The index mutex guards the one connection that writes (refresh, apply,
//! snapshots). A list query does not need it for longer than the refresh: it
//! runs on a pooled read connection, so concurrent queries run side by side
//! (the database is in WAL mode) instead of one after another.
//!
//! A full `reindex` replaces the database file. [`Readers::gate`] keeps the
//! two apart: queries hold it shared while they use a connection, the rebuild
//! holds it exclusively, so no connection is open across the replacement and
//! none is opened while the file is missing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use camino::Utf8PathBuf;
use clove_index::{Index, IndexError};

use crate::fresh::{file_id, FileId};

/// Idle connections kept per project.
const KEEP: usize = 4;

pub struct Readers {
    db_path: Utf8PathBuf,
    idle: Mutex<Vec<(u64, Index)>>,
    /// Bumped whenever `index.db` is a new file: older connections are closed
    /// instead of reused.
    generation: AtomicU64,
    /// The file the connections are open on.
    db_id: Mutex<Option<FileId>>,
    /// Shared by a query for as long as it uses a connection; exclusive
    /// across a rebuild. Taken before the index mutex.
    pub gate: RwLock<()>,
}

/// A read connection on loan from the pool.
pub struct Reader {
    generation: u64,
    pub index: Index,
}

impl Readers {
    pub fn new(db_path: Utf8PathBuf) -> Readers {
        let db_id = file_id(db_path.as_std_path());
        Readers {
            db_path,
            idle: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
            db_id: Mutex::new(db_id),
            gate: RwLock::new(()),
        }
    }

    /// Borrow a read connection. Call it with the index mutex held, so it
    /// reads at least what the refresh just wrote and never opens the file
    /// while a rebuild has it removed.
    pub fn take(&self) -> Result<Reader, IndexError> {
        let generation = self.generation.load(Ordering::Relaxed);
        let mut idle = lock(&self.idle);
        while let Some((born, index)) = idle.pop() {
            if born == generation {
                return Ok(Reader { generation, index });
            }
        }
        drop(idle);
        Ok(Reader {
            generation,
            index: Index::open(&self.db_path)?,
        })
    }

    /// Return a connection; it is closed if the file was replaced meanwhile.
    pub fn give(&self, reader: Reader) {
        if reader.generation != self.generation.load(Ordering::Relaxed) {
            return;
        }
        let mut idle = lock(&self.idle);
        if idle.len() < KEEP {
            idle.push((reader.generation, reader.index));
        }
    }

    /// `index.db` is a new file from now on: close every idle connection.
    pub fn reset(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        lock(&self.idle).clear();
        *lock(&self.db_id) = file_id(self.db_path.as_std_path());
    }

    /// If another process replaced `index.db` (a `clove reindex` that did not
    /// go through this daemon), move `index` to the file that is there now.
    /// Otherwise the daemon would keep reading, and writing its snapshots
    /// into, a file that no longer has a name. Returns whether it moved.
    pub fn follow(&self, index: &mut Index) -> bool {
        // Missing right now (mid-replacement) or no identity on this
        // platform: stay where we are.
        let Some(now) = file_id(self.db_path.as_std_path()) else {
            return false;
        };
        if *lock(&self.db_id) == Some(now) {
            return false;
        }
        match Index::open(&self.db_path) {
            Ok(fresh) => {
                *index = fresh;
                self.reset();
                true
            }
            Err(_) => false,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let issues = root.join("issues");
        std::fs::create_dir_all(&issues).unwrap();
        let path = root.join("index.db");
        clove_index::reindex(&issues, &path).unwrap();
        (tmp, path)
    }

    #[test]
    fn several_readers_are_out_at_once_and_reused() {
        let (_tmp, path) = db();
        let readers = Readers::new(path);
        let (a, b) = (readers.take().unwrap(), readers.take().unwrap());
        assert_eq!(a.index.item_count().unwrap(), 0);
        assert_eq!(b.index.item_count().unwrap(), 0);
        readers.give(a);
        readers.give(b);
        assert_eq!(lock(&readers.idle).len(), 2);
        let _again = readers.take().unwrap();
        assert_eq!(
            lock(&readers.idle).len(),
            1,
            "a pooled connection is reused"
        );
    }

    #[test]
    fn a_reader_from_before_a_reset_is_not_pooled_again() {
        let (_tmp, path) = db();
        let readers = Readers::new(path);
        let old = readers.take().unwrap();
        readers.give(readers.take().unwrap());
        readers.reset();
        assert!(
            lock(&readers.idle).is_empty(),
            "a reset closes idle readers"
        );
        readers.give(old);
        assert!(
            lock(&readers.idle).is_empty(),
            "the old file's reader is closed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_write_connection_follows_a_replaced_database() {
        // Another process puts a new file under the same name.
        let (_tmp, path) = db();
        let readers = Readers::new(path.clone());
        let mut index = Index::open(&path).unwrap();
        assert!(!readers.follow(&mut index), "nothing was replaced yet");

        let issues = path.with_file_name("issues");
        std::fs::write(
            issues.join("proj-AAAAAAAA.md"),
            "---\nschema: 1\nid: proj-AAAAAAAA\ntitle: t\nstatus: open\ntype: feature\n\
             priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\nbody\n",
        )
        .unwrap();
        // Build the new index beside the old one and rename it over the name,
        // so the test does not depend on how `reindex` writes its target.
        let fresh = path.with_file_name("index.db.new");
        clove_index::reindex(&issues, &fresh).unwrap();
        std::fs::rename(&fresh, &path).unwrap();
        assert_eq!(index.item_count().unwrap(), 0, "still on the old file");

        assert!(readers.follow(&mut index));
        assert_eq!(index.item_count().unwrap(), 1);
        assert!(!readers.follow(&mut index), "now current");
    }
}
