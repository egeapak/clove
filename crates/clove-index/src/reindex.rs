//! Full index rebuild (T-S04, library half — DESIGN §6.6).
//!
//! [`reindex`] builds a fresh database at `index.db.tmp`, then atomically renames
//! it over `index.db`, so a crash mid-rebuild never corrupts the live index. A
//! `reindex.lock` advisory lock (released automatically if the process dies)
//! prevents concurrent rebuilds. The CLI command wrapper (`clove reindex`,
//! T-S04 CLI half) is built once the M0 command surface exists.

use std::time::Instant;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::{parse_item_bytes, GraphStore};
use clove_types::{CloveId, Item};
use rayon::prelude::*;
use rusqlite::{params, Connection, TransactionBehavior};

use crate::db::{IndexError, SCHEMA_VERSION};
use crate::write::{content_hash8, write_row, RowMeta};

/// Parse in parallel only above this many files (matches the file store's
/// threshold; below it rayon's wake-up cost dominates).
const PARALLEL_THRESHOLD: usize = 500;

/// Insert in transactions of this many rows so a huge repo does not hold one
/// unbounded statement cache / journal in memory at once.
const BATCH_SIZE: usize = 500;

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
    content_hash: [u8; 8],
}

/// Rebuild the index at `db_path` from the item files in `issues_dir`.
pub fn reindex(issues_dir: &Utf8Path, db_path: &Utf8Path) -> Result<ReindexReport, IndexError> {
    let start = Instant::now();
    let clove_dir = db_path.parent().unwrap_or(issues_dir);

    // Acquire the advisory lock for the whole rebuild. flock-based: the OS
    // releases it if this process dies, so a crash never leaves a stale lock.
    let lock_path = clove_dir.join("reindex.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)
        .map_err(|source| IndexError::IoError {
            path: lock_path.clone(),
            source,
        })?;
    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock.try_write().map_err(|_| IndexError::AlreadyRunning)?;

    // Build into a temp database next to the target.
    let tmp_path = Utf8PathBuf::from(format!("{db_path}.tmp"));
    remove_db_files(&tmp_path)?;

    // Carry the durable stats history (which shares this database) across the
    // atomic tmp→live rename, so a reindex never drops snapshots (M4).
    let preserved_snapshots = crate::stats_store::preserve_from(db_path);

    let mut warnings = Vec::new();
    let items_indexed = {
        let mut conn = Connection::open(&tmp_path).map_err(IndexError::SqliteError)?;
        init_build_conn(&conn)?;
        let parsed = parse_all(issues_dir, &mut warnings)?;
        write_all(&mut conn, &parsed)?;
        write_meta(&conn, issues_dir, clove_dir, parsed.len())?;
        crate::stats_store::insert_raw(&conn, &preserved_snapshots)?;
        // Build the covering index now, in one pass, rather than maintaining it
        // across every insert above (much cheaper for a bulk load).
        conn.execute_batch(crate::db::covering_index_ddl())?;
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA wal_checkpoint(TRUNCATE);")?;
        parsed.len()
    }; // conn dropped/closed here, flushing the WAL.

    // Atomically replace the live index.
    remove_db_files(db_path)?;
    std::fs::rename(tmp_path.as_std_path(), db_path.as_std_path()).map_err(|source| {
        IndexError::IoError {
            path: db_path.to_owned(),
            source,
        }
    })?;

    Ok(ReindexReport {
        items_indexed,
        duration_ms: start.elapsed().as_millis(),
        warnings,
    })
}

/// Open-time setup for the build connection: schema + throughput PRAGMAs.
fn init_build_conn(conn: &Connection) -> Result<(), IndexError> {
    // synchronous=OFF for raw build throughput; a crash here is detected via the
    // unset user_version / absent meta row, and only the discardable tmp db is
    // affected. Reset to NORMAL before checkpoint.
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=OFF;
         PRAGMA foreign_keys=ON;
         PRAGMA cache_size=-65536;",
    )?;
    conn.execute_batch(crate::db::schema_ddl())?;
    // The stats-history table shares this database; create it so the rebuilt
    // index can receive the preserved snapshot rows (M4).
    crate::stats_store::ensure_table(conn)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
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
        let item = parse_item_bytes(&bytes, &path, &id).map_err(|e| format!("{path}: {e}"))?;
        Ok(ParsedFile {
            item,
            file_name: name.clone(),
            mtime_ms: system_time_ms(meta.modified().ok()),
            mtime_ns: system_time_ns(meta.modified().ok()),
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
fn write_all(conn: &mut Connection, parsed: &[ParsedFile]) -> Result<(), IndexError> {
    let frontmatters: Vec<_> = parsed.iter().map(|p| p.item.frontmatter.clone()).collect();
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ranks = graph.topological_ranks();
    let excluded: std::collections::HashSet<CloveId> = graph.excluded_ids().into_iter().collect();

    for chunk in parsed.chunks(BATCH_SIZE) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        for pf in chunk {
            let id = &pf.item.frontmatter.id;
            let has_dangling_deps = graph
                .meta(id)
                .map(|m| m.has_dangling_deps())
                .unwrap_or(false);
            let meta = RowMeta {
                file_mtime_ms: pf.mtime_ms,
                content_hash: pf.content_hash,
                topo_rank: ranks.get(id).map(|r| *r as i64),
                has_dangling_deps,
                excluded: excluded.contains(id),
            };
            write_row(&tx, &pf.item, &meta)?;
            tx.execute(
                "INSERT OR REPLACE INTO file_mtimes (path, mtime_ns, content_hash) \
                 VALUES (?1, ?2, ?3)",
                params![pf.file_name, pf.mtime_ns, &pf.content_hash[..]],
            )?;
        }
        tx.commit()?;
    }
    Ok(())
}

/// Write the single `meta` oracle row last (DESIGN §6.6 step 8).
fn write_meta(
    conn: &Connection,
    issues_dir: &Utf8Path,
    clove_dir: &Utf8Path,
    file_count: usize,
) -> Result<(), IndexError> {
    let dir_mtime = system_time_ms(
        std::fs::metadata(issues_dir)
            .ok()
            .and_then(|m| m.modified().ok()),
    );
    let last_git_head = read_git_head(clove_dir);
    conn.execute(
        "INSERT OR REPLACE INTO meta (id, dir_mtime, file_count, last_git_head) \
         VALUES (1, ?1, ?2, ?3)",
        params![dir_mtime, file_count as i64, last_git_head],
    )?;
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
}
