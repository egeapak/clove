//! SQLite schema, connection lifecycle, and the row types (T-S01).
//!
//! [`Index`] owns a private [`rusqlite::Connection`]. The connection is never
//! handed out mutably: every write goes through the single encapsulated path in
//! [`crate::write::upsert_item`] (and the bulk path in [`crate::reindex`]), so
//! the FTS5 mirror can never silently drift from the `items` table (DESIGN §6.3,
//! T-S02). Schema version lives in `PRAGMA user_version`; on mismatch or
//! corruption the database is dropped and rebuilt (DESIGN §6.1).

use camino::{Utf8Path, Utf8PathBuf};
use rusqlite::Connection;
use smol_str::SmolStr;
use thiserror::Error;

/// Index schema version. Bumped whenever the DDL below changes incompatibly;
/// an open of an older/newer database triggers a drop-and-rebuild. v2 added the
/// `idx_items_list` covering index and the sentinel `topological_rank`. v3 added
/// `file_mtimes.synced_at` for the M3 daemon git auto-sync re-commit guard
/// (DESIGN §8.7); the index is a rebuildable cache, so the bump just rebuilds.
pub const SCHEMA_VERSION: i64 = 3;

/// Complete DDL for the index (DESIGN §6.1). Kept as one reviewable block.
/// PRAGMAs that must run per-connection (not persisted) are applied separately
/// in [`set_pragmas`].
///
/// Two deviations from the DESIGN §6.1 DDL, both forced by combining a
/// contentless FTS5 table with a `WITHOUT ROWID` `items` table:
///
/// 1. `items_fts` adds `contentless_delete=1`. The plan (T-S02) reached for the
///    FTS5 `'delete'` command, but that requires the *previous* column values to
///    undo a row — values we don't have when an item's body changed or its file
///    was removed. `contentless_delete=1` (SQLite ≥3.43, in the bundled build)
///    lets us delete a shadow row by rowid alone, correct across edits/deletes.
///
/// 2. A `fts_map(fts_rowid → item_id)` side table is added. A contentless FTS5
///    table returns NULL for all columns (even `UNINDEXED id`), and `items` has
///    no integer rowid to join on, so a full-text match cannot otherwise be
///    mapped back to an item id. `fts_map` is that mapping; it is tiny (one
///    integer + id per item) and preserves the contentless space win for bodies.
///
/// Both are maintained by [`crate::write::write_row`].
const SCHEMA_DDL: &str = "\
CREATE TABLE items (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    item_type TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 2,
    assignee TEXT,
    parent_id TEXT,
    topological_rank INTEGER,
    has_dangling_deps BOOLEAN NOT NULL DEFAULT FALSE,
    labels TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    closed_at TEXT,
    file_mtime INTEGER NOT NULL,
    content_hash BLOB NOT NULL,
    source_system TEXT,
    external_ref TEXT
) WITHOUT ROWID;

CREATE TABLE edges (
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    kind INTEGER NOT NULL,
    PRIMARY KEY (from_id, to_id, kind)
) WITHOUT ROWID;

CREATE TABLE labels (
    item_id TEXT NOT NULL,
    label TEXT NOT NULL,
    PRIMARY KEY (item_id, label)
) WITHOUT ROWID;

CREATE VIRTUAL TABLE items_fts USING fts5(
    id UNINDEXED,
    title,
    body,
    content='',
    contentless_delete=1,
    tokenize='ascii'
);

CREATE TABLE meta (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    dir_mtime INTEGER NOT NULL,
    file_count INTEGER NOT NULL,
    last_git_head TEXT
);

CREATE TABLE file_mtimes (
    path TEXT PRIMARY KEY,
    mtime_ns INTEGER NOT NULL,
    content_hash BLOB NOT NULL,
    synced_at INTEGER
);

CREATE TABLE fts_map (
    fts_rowid INTEGER PRIMARY KEY,
    item_id TEXT NOT NULL
);

CREATE INDEX idx_items_status ON items(status);
CREATE INDEX idx_edges_to ON edges(to_id, kind);
CREATE INDEX idx_edges_from ON edges(from_id, kind);
CREATE INDEX idx_labels_label ON labels(label);
";

/// The covering index for the lean `ls` projection, created separately so the
/// reindex bulk-load can defer it until after the rows are inserted (building it
/// once is far cheaper than maintaining it across 10k inserts).
///
/// Ordered by the list sort key `(priority, topological_rank, id)` and carrying
/// `status`/`item_type`/`title`, so the list query is an **index-only scan** — no
/// per-row lookup into the `WITHOUT ROWID` `items` b-tree (`EXPLAIN QUERY PLAN`:
/// `SCAN items USING COVERING INDEX idx_items_list`). This is why
/// `topological_rank` is stored as a sentinel, never `NULL` (see write.rs).
const COVERING_INDEX_DDL: &str = "CREATE INDEX idx_items_list \
    ON items(priority, topological_rank, id, status, item_type, title);";

/// The covering-index DDL, for the reindex builder (deferred) and `Index::open`.
pub(crate) fn covering_index_ddl() -> &'static str {
    COVERING_INDEX_DDL
}

/// The schema DDL, for the reindex builder which initializes a fresh tmp
/// database directly (T-S04) rather than through [`Index::open`].
pub(crate) fn schema_ddl() -> &'static str {
    SCHEMA_DDL
}

/// The explicit `items` column list, in a fixed order shared by every read path
/// so a single row mapper ([`ItemRow::from_row`]) can decode them.
pub(crate) const ITEM_COLUMNS: &str = "\
id, title, status, item_type, priority, assignee, parent_id, topological_rank, \
has_dangling_deps, labels, created_at, updated_at, closed_at, source_system, external_ref";

/// A failure originating in the index layer (DESIGN §6, T-S01).
#[derive(Debug, Error)]
pub enum IndexError {
    /// An underlying SQLite failure that is not a recoverable corruption.
    #[error("sqlite error: {0}")]
    SqliteError(#[from] rusqlite::Error),

    /// The on-disk `user_version` does not match [`SCHEMA_VERSION`].
    #[error("index schema version mismatch: found {found}, expected {expected}")]
    SchemaMismatch { found: i64, expected: i64 },

    /// The database file is corrupt or not a SQLite database; callers rebuild.
    #[error("index is corrupt: {0}")]
    CorruptIndex(String),

    /// A filesystem error working with the index file.
    #[error("i/o error at `{path}`: {source}")]
    IoError {
        path: Utf8PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Another `reindex` holds the lock (DESIGN §6.6); the caller should retry
    /// later rather than corrupt the in-progress rebuild.
    #[error("a reindex is already running")]
    AlreadyRunning,
}

impl IndexError {
    fn io(path: &Utf8Path, source: std::io::Error) -> IndexError {
        IndexError::IoError {
            path: path.to_owned(),
            source,
        }
    }
}

/// True for SQLite errors that mean the file is unusable and should be rebuilt
/// rather than surfaced (`SQLITE_CORRUPT` and "file is not a database").
pub(crate) fn is_corrupt(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _)
            if matches!(
                e.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            )
    )
}

/// A row of the `items` table as stored in the index.
///
/// String-typed to mirror the SQLite columns exactly; the CLI maps these into
/// the public JSON shape and applies `--fields` projection. `topological_rank`
/// is carried for ordering but never serialized into the public schema (T-S07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRow {
    pub id: String,
    pub title: String,
    pub status: String,
    pub item_type: String,
    pub priority: u8,
    pub assignee: Option<String>,
    pub parent_id: Option<String>,
    pub topological_rank: Option<i64>,
    pub has_dangling_deps: bool,
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub source_system: Option<String>,
    pub external_ref: Option<String>,
}

impl ItemRow {
    /// Decode a row selected with [`ITEM_COLUMNS`] (column order matters).
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ItemRow> {
        let labels_json: String = row.get(9)?;
        let labels = serde_json::from_str(&labels_json).unwrap_or_default();
        Ok(ItemRow {
            id: row.get(0)?,
            title: row.get(1)?,
            status: row.get(2)?,
            item_type: row.get(3)?,
            priority: row.get(4)?,
            assignee: row.get(5)?,
            parent_id: row.get(6)?,
            topological_rank: row.get(7)?,
            has_dangling_deps: row.get(8)?,
            labels,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
            closed_at: row.get(12)?,
            source_system: row.get(13)?,
            external_ref: row.get(14)?,
        })
    }
}

/// The lean column list for list views — just what `clove ls` renders. Selecting
/// only these (vs. all of [`ITEM_COLUMNS`]) is what lets the index `ls` path hit
/// its latency budget: no per-row label-JSON parse and ~3× fewer allocations.
pub(crate) const LIST_COLUMNS: &str = "id, status, item_type, priority, title";

/// A lean list row (the `clove ls` projection): only the columns the list
/// renders, so there is no per-row label-JSON parse and far fewer allocations
/// than [`ItemRow`].
///
/// The short, low-cardinality columns use [`SmolStr`], which stores strings up
/// to 23 bytes inline — so `id` (e.g. `proj-7af3q2k9`), `status`, and `type`
/// cost **no per-row heap allocation**. Only `title` heap-allocates. This roughly
/// quarters the per-row heap footprint and allocation count versus all-`String`
/// (see `tests/memory_footprint.rs`); the time win is small (the floor is
/// SQLite's per-row step cost) but the memory win is real at scale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemListRow {
    pub id: SmolStr,
    pub status: SmolStr,
    pub item_type: SmolStr,
    pub priority: u8,
    pub title: String,
}

impl ItemListRow {
    /// Decode a row selected with [`LIST_COLUMNS`] (column order matters). Uses
    /// `get_ref` + `as_str` to borrow each text column straight from the SQLite
    /// row buffer (no intermediate `String`) before inlining into `SmolStr`.
    pub(crate) fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ItemListRow> {
        Ok(ItemListRow {
            id: SmolStr::new(row.get_ref(0)?.as_str()?),
            status: SmolStr::new(row.get_ref(1)?.as_str()?),
            item_type: SmolStr::new(row.get_ref(2)?.as_str()?),
            priority: row.get(3)?,
            title: row.get(4)?,
        })
    }
}

/// A handle to an opened SQLite index.
#[derive(Debug)]
pub struct Index {
    conn: Connection,
}

impl Index {
    /// Open an existing index, or initialize the schema if the file is brand new.
    ///
    /// Returns [`IndexError::SchemaMismatch`] on a version mismatch and
    /// [`IndexError::CorruptIndex`] (or a corrupt [`IndexError::SqliteError`])
    /// on an unreadable file; prefer [`Index::open_or_create`], which recovers
    /// from both by rebuilding.
    pub fn open(path: &Utf8Path) -> Result<Index, IndexError> {
        let conn = Connection::open(path).map_err(|e| {
            if is_corrupt(&e) {
                IndexError::CorruptIndex(e.to_string())
            } else {
                IndexError::SqliteError(e)
            }
        })?;
        set_pragmas(&conn)?;

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        match version {
            0 => {
                conn.execute_batch(SCHEMA_DDL)?;
                conn.execute_batch(COVERING_INDEX_DDL)?;
                conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            }
            v if v == SCHEMA_VERSION => {}
            found => {
                return Err(IndexError::SchemaMismatch {
                    found,
                    expected: SCHEMA_VERSION,
                })
            }
        }
        Ok(Index { conn })
    }

    /// The public entry point: open the index, rebuilding from scratch if it is
    /// the wrong schema version or corrupt (DESIGN §6.1). A corrupt file is
    /// logged to stderr before rebuilding.
    pub fn open_or_create(path: &Utf8Path) -> Result<Index, IndexError> {
        match Index::open(path) {
            Ok(index) => Ok(index),
            Err(IndexError::SchemaMismatch { found, expected }) => {
                eprintln!(
                    "note: index schema changed (found {found}, expected {expected}); rebuilding {path}"
                );
                remove_db_files(path)?;
                Index::open(path)
            }
            Err(IndexError::CorruptIndex(msg)) => {
                eprintln!("warning: index at {path} is corrupt ({msg}); rebuilding");
                remove_db_files(path)?;
                Index::open(path)
            }
            Err(IndexError::SqliteError(e)) if is_corrupt(&e) => {
                eprintln!("warning: index at {path} is corrupt ({e}); rebuilding");
                remove_db_files(path)?;
                Index::open(path)
            }
            Err(other) => Err(other),
        }
    }

    /// Borrow the connection for reads (queries, staleness checks). Not a write
    /// path — mutations go through [`Index::upsert_item`].
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Internal mutable access for the crate's own encapsulated write paths.
    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Number of indexed items (diagnostic / test helper).
    pub fn item_count(&self) -> Result<usize, IndexError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Flush the WAL back into the main database file and truncate it. Run on
    /// daemon shutdown (DESIGN §8.9) so no `-wal` work is left dangling for the
    /// next opener.
    pub fn checkpoint_truncate(&self) -> Result<(), IndexError> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }
}

/// Apply the per-connection PRAGMAs (DESIGN §6.1). `execute_batch` tolerates the
/// row returned by `journal_mode`.
fn set_pragmas(conn: &Connection) -> Result<(), IndexError> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA busy_timeout=5000;
         PRAGMA cache_size=-65536;",
    )?;
    Ok(())
}

/// Remove the database and its WAL/SHM sidecar files (best-effort: missing
/// sidecars are not an error).
fn remove_db_files(path: &Utf8Path) -> Result<(), IndexError> {
    for suffix in ["", "-wal", "-shm"] {
        let p = Utf8PathBuf::from(format!("{path}{suffix}"));
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(IndexError::io(&p, e)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_db() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("index.db")).unwrap();
        (dir, path)
    }

    #[test]
    fn opens_empty_then_reopens() {
        let (_dir, path) = tmp_db();
        {
            let index = Index::open_or_create(&path).unwrap();
            assert_eq!(index.item_count().unwrap(), 0);
        }
        // Reopen from a "previous session": schema already present, version OK.
        let index = Index::open_or_create(&path).unwrap();
        assert_eq!(index.item_count().unwrap(), 0);
    }

    #[test]
    fn wrong_schema_version_triggers_rebuild() {
        let (_dir, path) = tmp_db();
        {
            let index = Index::open(&path).unwrap();
            index
                .conn()
                .pragma_update(None, "user_version", 999_i64)
                .unwrap();
        }
        // Plain open reports the mismatch...
        match Index::open(&path) {
            Err(IndexError::SchemaMismatch { found, expected }) => {
                assert_eq!(found, 999);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
        // ...and open_or_create transparently rebuilds at the current version.
        let index = Index::open_or_create(&path).unwrap();
        let v: i64 = index
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn corrupt_file_triggers_rebuild() {
        let (_dir, path) = tmp_db();
        // Write bytes that are not a valid SQLite database.
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"this is definitely not sqlite").unwrap();
        }
        let index = Index::open_or_create(&path).unwrap();
        assert_eq!(index.item_count().unwrap(), 0);
    }

    #[test]
    fn schema_has_expected_tables() {
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();
        let count: i64 = index
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN \
                 ('items','edges','labels','meta','file_mtimes')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 5);
    }

    /// T-D01 / schema v3: `file_mtimes` carries the nullable `synced_at` column
    /// the M3 daemon git auto-sync uses to suppress the re-commit feedback loop
    /// (DESIGN §8.7). A fresh open must expose it.
    #[test]
    fn file_mtimes_has_synced_at_column() {
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();
        let has_col: bool = index
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('file_mtimes') \
                 WHERE name = 'synced_at'",
                [],
                |r| r.get::<_, i64>(0).map(|n| n == 1),
            )
            .unwrap();
        assert!(has_col, "file_mtimes.synced_at must exist at schema v3");
        assert_eq!(SCHEMA_VERSION, 3, "M3 ships index schema v3");
    }
}
