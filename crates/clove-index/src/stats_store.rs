//! Stats snapshot history, stored **in the index database** (`clove stats
//! --snapshot`/`--history`, M4).
//!
//! Analytics snapshots live in a `snapshots` table inside `index.db` — one
//! database for the whole tool, rather than a second file. The index is a
//! rebuildable cache, though, so the two destructive cache operations are taught
//! to carry the durable `snapshots` table across them:
//!
//! - a full [`crate::reindex`] (tmp-build + atomic rename) copies snapshot rows
//!   into the new database before the rename;
//! - schema-mismatch recovery in [`crate::db`] reads the rows out before the
//!   drop-and-rebuild and reinserts them after.
//!
//! True file corruption (the file cannot be read at all) is the one case where
//! history is lost — acceptable, since snapshots are non-mandatory analytics and
//! the item files remain the source of truth.
//!
//! Each snapshot stores the headline scalar metrics as columns (so trend queries
//! are plain SQL) plus the full [`StatsReport`] as a JSON blob (so the rich
//! breakdowns survive a round-trip). The table is created on demand: a repo that
//! never snapshots carries an empty table at worst.

use camino::Utf8Path;
use chrono::{DateTime, Utc};
use clove_core::StatsReport;
use rusqlite::{params, Connection};

use crate::db::{Index, IndexError};

/// DDL for the snapshot history table. Idempotent (`IF NOT EXISTS`) so it can run
/// on every open without a schema-version bump; the index's own `user_version`
/// continues to govern only the rebuildable cache tables.
const SNAPSHOTS_DDL: &str = "\
CREATE TABLE IF NOT EXISTS snapshots (
    id INTEGER PRIMARY KEY,
    captured_at TEXT NOT NULL,
    total INTEGER NOT NULL,
    open INTEGER NOT NULL,
    in_progress INTEGER NOT NULL,
    closed INTEGER NOT NULL,
    ready INTEGER NOT NULL,
    blocked INTEGER NOT NULL,
    dangling INTEGER NOT NULL,
    cycles INTEGER NOT NULL,
    detail_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_snapshots_captured ON snapshots(captured_at);
";

/// Ensure the `snapshots` table exists on `conn`. Called from [`Index::open`] and
/// the reindex build path so every opened/rebuilt index can hold history.
pub(crate) fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SNAPSHOTS_DDL)
}

/// A raw snapshot row (every column but the autoincrement `id`). Used to carry
/// history across a reindex or schema-mismatch rebuild without going through the
/// `StatsReport` type (a verbatim row copy, robust to report-shape changes).
#[derive(Debug, Clone)]
pub(crate) struct RawSnapshot {
    captured_at: String,
    total: i64,
    open: i64,
    in_progress: i64,
    closed: i64,
    ready: i64,
    blocked: i64,
    dangling: i64,
    cycles: i64,
    detail_json: String,
}

/// Read every snapshot row from `conn` (oldest first by id). Returns an empty
/// vec if the table is absent — callers preserving rows treat "no table" as "no
/// history".
pub(crate) fn read_raw(conn: &Connection) -> rusqlite::Result<Vec<RawSnapshot>> {
    let table_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='snapshots'",
        [],
        |r| r.get::<_, i64>(0).map(|n| n > 0),
    )?;
    if !table_exists {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT captured_at, total, open, in_progress, closed, ready, blocked, \
         dangling, cycles, detail_json FROM snapshots ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(RawSnapshot {
            captured_at: row.get(0)?,
            total: row.get(1)?,
            open: row.get(2)?,
            in_progress: row.get(3)?,
            closed: row.get(4)?,
            ready: row.get(5)?,
            blocked: row.get(6)?,
            dangling: row.get(7)?,
            cycles: row.get(8)?,
            detail_json: row.get(9)?,
        })
    })?;
    rows.collect()
}

/// Best-effort read of the snapshots at `db_path`. Any failure (missing file,
/// corrupt database, missing table) yields an empty vec — preserving history is
/// a courtesy, never a reason to fail a reindex/rebuild.
pub(crate) fn preserve_from(db_path: &Utf8Path) -> Vec<RawSnapshot> {
    if !db_path.exists() {
        return Vec::new();
    }
    Connection::open(db_path)
        .and_then(|conn| read_raw(&conn))
        .unwrap_or_default()
}

/// Reinsert preserved snapshot rows into `conn` (which must already have the
/// table). Ids are reassigned; capture order is preserved.
pub(crate) fn insert_raw(conn: &Connection, rows: &[RawSnapshot]) -> rusqlite::Result<()> {
    for r in rows {
        conn.execute(
            "INSERT INTO snapshots \
             (captured_at, total, open, in_progress, closed, ready, blocked, dangling, cycles, detail_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                r.captured_at,
                r.total,
                r.open,
                r.in_progress,
                r.closed,
                r.ready,
                r.blocked,
                r.dangling,
                r.cycles,
                r.detail_json,
            ],
        )?;
    }
    Ok(())
}

/// One recorded analytics snapshot: when it was taken plus the full report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// RFC3339 capture time (UTC).
    pub captured_at: String,
    /// The full analytics report as recorded.
    pub report: StatsReport,
}

impl Index {
    /// Append one analytics snapshot stamped `captured_at` to the index's history.
    pub fn record_snapshot(
        &self,
        captured_at: DateTime<Utc>,
        report: &StatsReport,
    ) -> Result<(), IndexError> {
        ensure_table(self.conn())?;
        let detail_json = serde_json::to_string(report).map_err(|e| {
            IndexError::CorruptIndex(format!("failed to serialize stats report: {e}"))
        })?;
        self.conn().execute(
            "INSERT INTO snapshots \
             (captured_at, total, open, in_progress, closed, ready, blocked, dangling, cycles, detail_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                captured_at.to_rfc3339(),
                report.total,
                report.by_status.open,
                report.by_status.in_progress,
                report.by_status.closed,
                report.ready,
                report.blocked,
                report.dangling,
                report.cycles,
                detail_json,
            ],
        )?;
        Ok(())
    }

    /// Read recorded snapshots, most recent first. `since` (an RFC3339 lower
    /// bound, inclusive) and `limit` are optional; `None`/`0` mean unbounded.
    ///
    /// `captured_at` is stored via [`chrono::DateTime::to_rfc3339`], which always
    /// renders the `+00:00` UTC offset; the `WHERE captured_at >= ?` comparison is
    /// lexicographic. So a `since` bound in any other equivalent form (e.g. the
    /// `Z` suffix, `2026-06-03T00:00:00Z`) is first re-rendered to the same
    /// canonical `to_rfc3339` form, so the string comparison agrees with the
    /// instant comparison. An unparseable `since` is used verbatim (best effort).
    pub fn snapshot_history(
        &self,
        since: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<StatsSnapshot>, IndexError> {
        ensure_table(self.conn())?;
        let since_canonical: Option<String> = since.map(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&chrono::Utc).to_rfc3339())
                .unwrap_or_else(|_| s.to_owned())
        });

        let mut sql = String::from("SELECT captured_at, detail_json FROM snapshots");
        if since_canonical.is_some() {
            sql.push_str(" WHERE captured_at >= ?1");
        }
        sql.push_str(" ORDER BY captured_at DESC, id DESC");
        if let Some(n) = limit.filter(|&n| n > 0) {
            sql.push_str(&format!(" LIMIT {n}"));
        }

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(String, String)> {
            Ok((row.get(0)?, row.get(1)?))
        };
        let rows = match &since_canonical {
            Some(s) => stmt.query_map([s], map_row)?,
            None => stmt.query_map([], map_row)?,
        };

        let mut out = Vec::new();
        for row in rows {
            let (captured_at, detail_json) = row?;
            let report: StatsReport = serde_json::from_str(&detail_json).map_err(|e| {
                IndexError::CorruptIndex(format!("corrupt snapshot at {captured_at}: {e}"))
            })?;
            out.push(StatsSnapshot {
                captured_at,
                report,
            });
        }
        Ok(out)
    }

    /// Number of recorded snapshots (diagnostic / test helper).
    pub fn snapshot_count(&self) -> Result<usize, IndexError> {
        ensure_table(self.conn())?;
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM snapshots", [], |r| r.get(0))?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;
    use clove_core::{compute_stats, GraphStore, StatsOptions};

    fn empty_report() -> StatsReport {
        let (graph, _) = GraphStore::build(&[]);
        compute_stats(&[], &graph, Utc::now(), StatsOptions::default())
    }

    fn tmp_db() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("index.db")).unwrap();
        (dir, path)
    }

    #[test]
    fn record_and_read_back() {
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();
        assert_eq!(index.snapshot_count().unwrap(), 0);

        let mut report = empty_report();
        report.total = 7;
        report.ready = 3;
        let t: DateTime<Utc> = "2026-06-01T00:00:00Z".parse().unwrap();
        index.record_snapshot(t, &report).unwrap();

        let hist = index.snapshot_history(None, None).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].report.total, 7);
        assert_eq!(hist[0].report.ready, 3);
    }

    #[test]
    fn history_orders_desc_and_filters_since() {
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();

        for (i, ts) in [
            "2026-06-01T00:00:00Z",
            "2026-06-02T00:00:00Z",
            "2026-06-03T00:00:00Z",
        ]
        .iter()
        .enumerate()
        {
            let mut report = empty_report();
            report.total = i as u64;
            index.record_snapshot(ts.parse().unwrap(), &report).unwrap();
        }

        let all = index.snapshot_history(None, None).unwrap();
        let times: Vec<&str> = all.iter().map(|s| s.captured_at.as_str()).collect();
        assert_eq!(times[0], "2026-06-03T00:00:00+00:00", "{times:?}");

        let since = index
            .snapshot_history(Some("2026-06-02T00:00:00+00:00"), None)
            .unwrap();
        assert_eq!(since.len(), 2);

        // A `Z`-suffixed bound is equivalent to the stored `+00:00` form and must
        // include the boundary snapshot (regression: naive lexicographic compare
        // would drop it because '+' < 'Z').
        let since_z = index
            .snapshot_history(Some("2026-06-02T00:00:00Z"), None)
            .unwrap();
        assert_eq!(
            since_z.len(),
            2,
            "Z-form --since must match the +00:00 store"
        );

        let limited = index.snapshot_history(None, Some(1)).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].report.total, 2);
    }

    #[test]
    fn reopen_preserves_history() {
        let (_dir, path) = tmp_db();
        {
            let index = Index::open(&path).unwrap();
            index.record_snapshot(Utc::now(), &empty_report()).unwrap();
        }
        // A second open must NOT wipe history (it lives in the index file now).
        let index = Index::open(&path).unwrap();
        assert_eq!(index.snapshot_count().unwrap(), 1);
    }

    #[test]
    fn preserve_roundtrip_across_files() {
        // Simulate the reindex carry-over: read from one db, insert into another.
        let (_dir, src) = tmp_db();
        {
            let index = Index::open(&src).unwrap();
            let mut report = empty_report();
            report.total = 5;
            index.record_snapshot(Utc::now(), &report).unwrap();
        }
        let preserved = preserve_from(&src);
        assert_eq!(preserved.len(), 1);

        let dir = tempfile::tempdir().unwrap();
        let dst = Utf8PathBuf::from_path_buf(dir.path().join("fresh.db")).unwrap();
        let dst_index = Index::open(&dst).unwrap();
        insert_raw(dst_index.conn(), &preserved).unwrap();
        let hist = dst_index.snapshot_history(None, None).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].report.total, 5);
    }

    #[test]
    fn preserve_from_missing_or_empty_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = Utf8PathBuf::from_path_buf(dir.path().join("nope.db")).unwrap();
        assert!(preserve_from(&missing).is_empty());
    }

    #[test]
    fn full_reindex_preserves_snapshots() {
        // The headline guarantee of the merged store: a full reindex (tmp-build +
        // atomic rename) must NOT drop the durable history that shares the file.
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let issues = root.join(".clove/issues");
        std::fs::create_dir_all(&issues).unwrap();
        std::fs::write(
            issues.join("proj-AAAAAAAA.md"),
            "---\nschema: 1\nid: proj-AAAAAAAA\ntitle: A\nstatus: open\ntype: feature\n\
             priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\nbody\n",
        )
        .unwrap();
        let db = root.join(".clove/index.db");

        // Build an index, record a snapshot.
        crate::reindex::reindex(&issues, &db).unwrap();
        {
            let index = Index::open(&db).unwrap();
            let mut report = empty_report();
            report.total = 99;
            index.record_snapshot(Utc::now(), &report).unwrap();
            assert_eq!(index.snapshot_count().unwrap(), 1);
        }

        // A second full reindex replaces the cache file via rename...
        crate::reindex::reindex(&issues, &db).unwrap();

        // ...and the snapshot is still there.
        let index = Index::open(&db).unwrap();
        assert_eq!(index.snapshot_count().unwrap(), 1);
        assert_eq!(
            index.snapshot_history(None, None).unwrap()[0].report.total,
            99
        );
    }
}
