//! Full index rebuild (T-S04, library half — DESIGN §6.6).
//!
//! [`reindex`] rebuilds the cache tables **inside the live database, in one
//! transaction**. A crash mid-rebuild rolls back to the old contents, readers
//! keep their snapshot until the commit, and a connection that was open before
//! the rebuild reads the new rows after it. The file at `index.db` is never
//! unlinked while it is usable: a reader that opens a missing path creates an
//! empty database there, and its `-wal` would then be replayed over whatever
//! file is moved into place. Only a file SQLite cannot read at all is replaced.
//!
//! A `reindex.lock` advisory lock (released automatically if the process dies)
//! prevents concurrent rebuilds. The CLI command wrapper is `clove reindex`.

use std::time::Instant;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::{parse_item_bytes, GraphStore};
use clove_types::{CloveId, Item};
use rayon::prelude::*;
use rusqlite::{params, Connection, TransactionBehavior};

use crate::db::IndexError;
use crate::write::{content_hash8, write_row, RowMeta};

/// Parse in parallel only above this many files (matches the file store's
/// threshold; below it rayon's wake-up cost dominates).
const PARALLEL_THRESHOLD: usize = 500;

/// The outcome of a rebuild.
#[derive(Debug, Clone)]
pub struct ReindexReport {
    /// Number of items written to the new index.
    pub items_indexed: usize,
    /// Wall-clock duration of the rebuild.
    pub duration_ms: u128,
    /// Non-fatal problems (e.g. files that failed to parse and were skipped).
    pub warnings: Vec<String>,
}

/// A successfully parsed item plus the file metadata the index needs.
struct ParsedFile {
    item: Item,
    file_name: String,
    mtime_ms: i64,
    mtime_ns: i64,
    file_size: i64,
    content_hash: [u8; 8],
}

/// Rebuild the index at `db_path` from the item files in `issues_dir`.
pub fn reindex(issues_dir: &Utf8Path, db_path: &Utf8Path) -> Result<ReindexReport, IndexError> {
    let start = Instant::now();
    let clove_dir = db_path.parent().unwrap_or(issues_dir);

    // Acquire the advisory lock for the whole rebuild. flock-based: the OS
    // releases it if this process dies, so a crash never leaves a stale lock.
    let lock_path = clove_dir.join("reindex.lock");
    let lock_file =
        clove_core::fs_safe::open_lock_file(&lock_path).map_err(|source| IndexError::IoError {
            path: lock_path.clone(),
            source,
        })?;
    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock.try_write().map_err(|_| IndexError::AlreadyRunning)?;

    // Everything read from the files happens before the write transaction, so
    // the database is locked only for the inserts. The directory mtime is taken
    // before the scan: it becomes the staleness oracle and must not be newer
    // than what the scan saw (see `stale::check_staleness`).
    let dir_mtime = system_time_ms(
        std::fs::metadata(issues_dir)
            .ok()
            .and_then(|m| m.modified().ok()),
    );
    let mut warnings = Vec::new();
    let parsed = parse_all(issues_dir, &mut warnings)?;
    let meta = MetaRow {
        dir_mtime,
        file_count: parsed.len() as i64,
        last_git_head: read_git_head(clove_dir),
    };

    match rebuild_in_place(db_path, &parsed, &meta) {
        Ok(()) => {}
        Err(e) if unreadable(&e) => {
            // The file is not a database SQLite can work with, so there is
            // nothing to rebuild inside. Replace it; the history it held is
            // carried over as far as it can still be read.
            let preserved = crate::stats_store::preserve_from(db_path);
            remove_db_files(db_path)?;
            rebuild_in_place(db_path, &parsed, &meta)?;
            if !preserved.is_empty() {
                let conn = open_build_conn(db_path)?;
                crate::stats_store::insert_raw(&conn, &preserved)?;
            }
        }
        Err(e) => return Err(e),
    }

    Ok(ReindexReport {
        items_indexed: parsed.len(),
        duration_ms: start.elapsed().as_millis(),
        warnings,
    })
}

/// The single `meta` oracle row.
struct MetaRow {
    dir_mtime: i64,
    file_count: i64,
    last_git_head: Option<String>,
}

/// Whether `err` means the file cannot be used as a database at all.
fn unreadable(err: &IndexError) -> bool {
    match err {
        IndexError::CorruptIndex(_) => true,
        IndexError::SqliteError(e) => crate::db::is_corrupt(e),
        _ => false,
    }
}

/// Open the connection a rebuild writes through.
fn open_build_conn(db_path: &Utf8Path) -> Result<Connection, IndexError> {
    crate::db::refuse_symlinked_db(db_path)?;
    let conn = Connection::open(db_path).map_err(IndexError::SqliteError)?;
    crate::db::set_pragmas(&conn)?;
    Ok(conn)
}

/// Replace the cache tables of the database at `db_path` with `parsed`, in one
/// transaction. The `snapshots` history table is not touched.
fn rebuild_in_place(
    db_path: &Utf8Path,
    parsed: &[ParsedFile],
    meta: &MetaRow,
) -> Result<(), IndexError> {
    let mut conn = open_build_conn(db_path)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::db::reset_cache_schema(&tx)?;
    write_all(&tx, parsed)?;
    // The `meta` row goes last (DESIGN §6.6).
    tx.execute(
        "INSERT OR REPLACE INTO meta (id, dir_mtime, file_count, last_git_head) \
         VALUES (1, ?1, ?2, ?3)",
        params![meta.dir_mtime, meta.file_count, meta.last_git_head],
    )?;
    // Build the covering index now, in one pass, rather than maintaining it
    // across every insert above (much cheaper for a bulk load).
    tx.execute_batch(crate::db::covering_index_ddl())?;
    tx.commit()?;
    // Fold the rebuild out of the WAL. Best-effort: a reader holding an older
    // snapshot blocks the truncation, and the next checkpoint does it instead.
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    Ok(())
}

/// Parse every item file, in parallel above the threshold. Per-file failures are
/// recorded as warnings and skipped (malformed-skip rule).
fn parse_all(
    issues_dir: &Utf8Path,
    warnings: &mut Vec<String>,
) -> Result<Vec<ParsedFile>, IndexError> {
    let mut paths = Vec::new();
    let read_dir = std::fs::read_dir(issues_dir).map_err(|source| IndexError::IoError {
        path: issues_dir.to_owned(),
        source,
    })?;
    for entry in read_dir {
        let entry = entry.map_err(|source| IndexError::IoError {
            path: issues_dir.to_owned(),
            source,
        })?;
        let ft = entry.file_type().map_err(|source| IndexError::IoError {
            path: issues_dir.to_owned(),
            source,
        })?;
        if !ft.is_file() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name
            .strip_suffix(".md")
            .and_then(|s| CloveId::new(s).ok())
            .is_none()
        {
            continue;
        }
        paths.push(name);
    }

    let parse_one = |name: &String| -> Result<ParsedFile, String> {
        let stem = name.strip_suffix(".md").expect("filtered to .md");
        let id = CloveId::new(stem).expect("filtered to valid id");
        let path = issues_dir.join(name);
        let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
        let meta = std::fs::metadata(&path).map_err(|e| format!("{path}: {e}"))?;
        let mut item = parse_item_bytes(&bytes, &path, &id).map_err(|e| format!("{path}: {e}"))?;
        // The index stores no bodies. Dropping each one here, as soon as the
        // file is hashed, keeps the rebuild's memory at the size of the
        // frontmatter rather than the size of the store.
        item.body = String::new();
        Ok(ParsedFile {
            item,
            file_name: name.clone(),
            mtime_ms: system_time_ms(meta.modified().ok()),
            mtime_ns: system_time_ns(meta.modified().ok()),
            file_size: bytes.len() as i64,
            content_hash: content_hash8(&bytes),
        })
    };

    let results: Vec<Result<ParsedFile, String>> = if paths.len() > PARALLEL_THRESHOLD {
        paths.par_iter().map(parse_one).collect()
    } else {
        paths.iter().map(parse_one).collect()
    };

    let mut parsed = Vec::with_capacity(results.len());
    for r in results {
        match r {
            Ok(p) => parsed.push(p),
            Err(msg) => warnings.push(format!("skipped unparseable file: {msg}")),
        }
    }
    Ok(parsed)
}

/// Insert all parsed items, computing topological rank and dangling flags from
/// the dependency graph built over their frontmatters.
fn write_all(tx: &rusqlite::Transaction<'_>, parsed: &[ParsedFile]) -> Result<(), IndexError> {
    let frontmatters: Vec<_> = parsed.iter().map(|p| p.item.frontmatter.clone()).collect();
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ranks = graph.topological_ranks();
    let excluded: std::collections::HashSet<CloveId> = graph.excluded_ids().into_iter().collect();

    let mut mtimes = tx.prepare(
        "INSERT OR REPLACE INTO file_mtimes (path, mtime_ns, content_hash) VALUES (?1, ?2, ?3)",
    )?;
    for pf in parsed {
        let id = &pf.item.frontmatter.id;
        let has_dangling_deps = graph
            .meta(id)
            .map(|m| m.has_dangling_deps())
            .unwrap_or(false);
        let meta = RowMeta {
            file_mtime_ms: pf.mtime_ms,
            file_size: pf.file_size,
            content_hash: pf.content_hash,
            topo_rank: ranks.get(id).map(|r| *r as i64),
            has_dangling_deps,
            excluded: excluded.contains(id),
        };
        write_row(tx, &pf.item, &meta)?;
        mtimes.execute(params![pf.file_name, pf.mtime_ns, &pf.content_hash[..]])?;
    }
    Ok(())
}

/// Best-effort read of `.git/HEAD` (relative to the repo root, the parent of
/// `.clove/`) for checkout-detection on the next staleness pass (DESIGN §6.2).
fn read_git_head(clove_dir: &Utf8Path) -> Option<String> {
    let repo_root = clove_dir.parent()?;
    std::fs::read_to_string(repo_root.join(".git/HEAD")).ok()
}

fn system_time_ms(t: Option<std::time::SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn system_time_ns(t: Option<std::time::SystemTime>) -> i64 {
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Remove a database file and its WAL/SHM sidecars (missing files are fine).
fn remove_db_files(path: &Utf8Path) -> Result<(), IndexError> {
    for suffix in ["", "-wal", "-shm"] {
        let p = Utf8PathBuf::from(format!("{path}{suffix}"));
        match std::fs::remove_file(p.as_std_path()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(IndexError::IoError { path: p, source }),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Index;

    fn item_md(id: &str, body: &str) -> String {
        format!(
            "---\nschema: 1\nid: {id}\ntitle: T\nstatus: open\ntype: feature\n\
             priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\n{body}\n"
        )
    }

    fn id_for(i: usize) -> String {
        const ALPH: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        let mut n = i as u32;
        let mut buf = [b'0'; 8];
        let mut p = 8;
        while n > 0 && p > 0 {
            p -= 1;
            buf[p] = ALPH[(n % 32) as usize];
            n /= 32;
        }
        format!("proj-{}", String::from_utf8(buf.to_vec()).unwrap())
    }

    struct Fx {
        _dir: tempfile::TempDir,
        issues: Utf8PathBuf,
        db: Utf8PathBuf,
    }

    fn fx(n: usize) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let issues = root.join(".clove/issues");
        std::fs::create_dir_all(&issues).unwrap();
        for i in 0..n {
            let id = id_for(i);
            std::fs::write(
                issues.join(format!("{id}.md")),
                item_md(&id, &format!("body {i}")),
            )
            .unwrap();
        }
        let db = root.join(".clove/index.db");
        Fx {
            _dir: dir,
            issues,
            db,
        }
    }

    #[test]
    fn rebuilds_and_indexes_all_items() {
        let fx = fx(10);
        let report = reindex(&fx.issues, &fx.db).unwrap();
        assert_eq!(report.items_indexed, 10);
        assert!(fx.db.exists());

        let index = Index::open(&fx.db).unwrap();
        assert_eq!(index.item_count().unwrap(), 10);
    }

    #[test]
    fn skips_unparseable_file_with_warning() {
        let fx = fx(3);
        // A file with a valid id name but broken frontmatter.
        let bad = id_for(42);
        std::fs::write(
            fx.issues.join(format!("{bad}.md")),
            "not frontmatter at all",
        )
        .unwrap();

        let report = reindex(&fx.issues, &fx.db).unwrap();
        assert_eq!(report.items_indexed, 3);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    }

    #[test]
    fn topological_rank_reflects_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let issues = root.join(".clove/issues");
        std::fs::create_dir_all(&issues).unwrap();
        // a depends on b: b must come before a in topological order.
        let a = "proj-AAAAAAAA";
        let b = "proj-BBBBBBBB";
        std::fs::write(
            issues.join(format!("{a}.md")),
            format!(
                "---\nschema: 1\nid: {a}\ntitle: A\nstatus: open\ntype: feature\npriority: 2\n\
                 created: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\ndeps:\n  - {b}\n---\nbody\n"
            ),
        )
        .unwrap();
        std::fs::write(issues.join(format!("{b}.md")), item_md(b, "body b")).unwrap();
        let db = root.join(".clove/index.db");
        reindex(&issues, &db).unwrap();

        let index = Index::open(&db).unwrap();
        let rank = |id: &str| -> i64 {
            index
                .conn()
                .query_row(
                    "SELECT topological_rank FROM items WHERE id = ?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        // clove-core's toposort places an edge's source before its target, and
        // `a depends on b` is the edge a→b, so the dependent `a` ranks *before*
        // its dependency `b`. The index stores those same ranks (consistency
        // with the file path is the contract).
        assert!(
            rank(a) < rank(b),
            "dependent a should rank before dependency b"
        );
    }

    #[test]
    fn concurrent_reindex_is_rejected() {
        let fx = fx(2);
        // Hold the lock from a separate open file description.
        let lock_path = fx.db.parent().unwrap().join("reindex.lock");
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&lock_path)
            .unwrap();
        let mut held = fd_lock::RwLock::new(f);
        let _g = held.try_write().unwrap();

        match reindex(&fx.issues, &fx.db) {
            Err(IndexError::AlreadyRunning) => {}
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
    }

    /// A cloned repo can plant `reindex.lock` as a symlink to a user file;
    /// taking the lock must never truncate the target.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_reindex_lock_never_clobbers_its_target() {
        let fx = fx(1);
        let victim_dir = tempfile::tempdir().unwrap();
        let victim = victim_dir.path().join("precious.txt");
        std::fs::write(&victim, "precious").unwrap();
        std::os::unix::fs::symlink(&victim, fx.db.parent().unwrap().join("reindex.lock")).unwrap();
        let _ = reindex(&fx.issues, &fx.db);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
    }

    fn add_snapshot(db: &Utf8Path) {
        let conn = Connection::open(db).unwrap();
        conn.execute(
            "INSERT INTO snapshots (captured_at, total, open, in_progress, closed, ready, \
             blocked, dangling, cycles, detail_json) \
             VALUES ('2026-06-02T10:00:00Z', 7, 0, 0, 0, 0, 0, 0, 0, '{}')",
            [],
        )
        .unwrap();
    }

    fn snapshot_totals(db: &Utf8Path) -> Vec<i64> {
        let conn = Connection::open(db).unwrap();
        let mut stmt = conn.prepare("SELECT total FROM snapshots").unwrap();
        let rows = stmt.query_map([], |r| r.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }

    /// The rebuild happens inside the live file: the path never stops existing,
    /// and a connection that was open before the rebuild reads the new rows.
    #[test]
    fn rebuilds_inside_the_live_database() {
        let fx = fx(3);
        reindex(&fx.issues, &fx.db).unwrap();
        let before = Index::open(&fx.db).unwrap();
        assert_eq!(before.item_count().unwrap(), 3);
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&fx.db).unwrap().ino()
        };

        let id = id_for(77);
        std::fs::write(fx.issues.join(format!("{id}.md")), item_md(&id, "new")).unwrap();
        let report = reindex(&fx.issues, &fx.db).unwrap();
        assert_eq!(report.items_indexed, 4);

        assert_eq!(
            before.item_count().unwrap(),
            4,
            "a connection opened before the rebuild must read the rebuilt rows"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(&fx.db).unwrap().ino(),
                inode,
                "index.db was replaced instead of rebuilt in place"
            );
        }
        assert!(!Utf8PathBuf::from(format!("{}.tmp", fx.db)).exists());
        assert_eq!(
            Index::open(&fx.db).unwrap().integrity_check().unwrap(),
            None
        );
    }

    /// The stats history is not part of the cache: a rebuild leaves it alone,
    /// also when the cache schema version differs.
    #[test]
    fn snapshot_history_survives_a_rebuild() {
        let fx = fx(2);
        reindex(&fx.issues, &fx.db).unwrap();
        add_snapshot(&fx.db);
        reindex(&fx.issues, &fx.db).unwrap();
        assert_eq!(snapshot_totals(&fx.db), vec![7]);

        Connection::open(&fx.db)
            .unwrap()
            .pragma_update(None, "user_version", 3_i64)
            .unwrap();
        let index = Index::open_or_rebuild(&fx.db, &fx.issues).unwrap();
        assert_eq!(index.item_count().unwrap(), 2);
        drop(index);
        assert_eq!(snapshot_totals(&fx.db), vec![7]);
    }

    /// A file that is not a database is replaced, and the rebuild still succeeds.
    #[test]
    fn replaces_a_file_that_is_not_a_database() {
        let fx = fx(2);
        std::fs::write(&fx.db, "this is definitely not sqlite, not at all").unwrap();
        let report = reindex(&fx.issues, &fx.db).unwrap();
        assert_eq!(report.items_indexed, 2);
        assert_eq!(Index::open(&fx.db).unwrap().item_count().unwrap(), 2);
    }

    /// The index stores no bodies, so the rebuild must not hold them.
    #[test]
    fn parsed_items_carry_no_body() {
        let fx = fx(3);
        let mut warnings = Vec::new();
        let parsed = parse_all(&fx.issues, &mut warnings).unwrap();
        assert_eq!(parsed.len(), 3);
        assert!(parsed.iter().all(|p| p.item.body.is_empty()));
        // The hash and size still describe the whole file.
        let bytes = std::fs::read(fx.issues.join(&parsed[0].file_name)).unwrap();
        assert_eq!(parsed[0].content_hash, content_hash8(&bytes));
        assert_eq!(parsed[0].file_size, bytes.len() as i64);
    }

    /// A view or trigger in `index.db` makes the index answer differently from
    /// the files. `Index::open` reports it and a rebuild removes it.
    #[test]
    fn a_planted_trigger_or_view_is_reported_and_rebuilt_away() {
        for plant in [
            "CREATE TRIGGER evil AFTER INSERT ON items BEGIN \
             UPDATE items SET title = 'pwned' WHERE id = NEW.id; END;",
            "ALTER TABLE items RENAME TO real_items; \
             CREATE VIEW items AS SELECT * FROM real_items WHERE 0;",
        ] {
            let fx = fx(2);
            reindex(&fx.issues, &fx.db).unwrap();
            Connection::open(&fx.db)
                .unwrap()
                .execute_batch(plant)
                .unwrap();
            match Index::open(&fx.db) {
                Err(IndexError::CorruptIndex(_)) => {}
                other => panic!("expected CorruptIndex, got {other:?}"),
            }
            let index = Index::open_or_rebuild(&fx.db, &fx.issues).unwrap();
            assert_eq!(index.item_count().unwrap(), 2);
            let (strays, pwned): (i64, i64) = index
                .conn()
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM sqlite_master \
                             WHERE type IN ('trigger','view') OR name = 'real_items'), \
                            (SELECT COUNT(*) FROM items WHERE title = 'pwned')",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!((strays, pwned), (0, 0));
        }
    }

    /// Every connection runs with the untrusted-schema settings.
    #[test]
    fn connections_do_not_trust_the_schema() {
        use rusqlite::config::DbConfig;
        let fx = fx(1);
        reindex(&fx.issues, &fx.db).unwrap();
        let index = Index::open(&fx.db).unwrap();
        let conn = index.conn();
        assert!(!conn
            .db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA)
            .unwrap());
        assert!(conn.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE).unwrap());
    }
}
