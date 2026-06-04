//! The durable stats history store (`clove stats --snapshot`/`--history`, M4).
//!
//! Unlike [`crate::Index`], which is a **rebuildable cache** that is dropped and
//! recreated on any schema change or corruption, this is a small **durable**
//! SQLite database (`.clove/stats.db`) that records point-in-time analytics
//! snapshots. Open-time corruption or a version mismatch is reported, never
//! silently wiped — losing history would be a data loss, not a cache miss.
//!
//! Each snapshot stores the headline scalar metrics as columns (so trend queries
//! are plain SQL) plus the full [`StatsReport`] as a JSON blob (so the rich
//! breakdowns survive a round-trip). The store is created lazily: a repo that
//! never snapshots never grows a `stats.db`.

use camino::Utf8Path;
use chrono::{DateTime, Utc};
use clove_core::StatsReport;
use rusqlite::Connection;

use crate::db::{is_corrupt, IndexError};

/// Stats-store schema version. Bumped on an incompatible change to the DDL below;
/// unlike the index, a mismatch is surfaced (and migrated in a future version),
/// never resolved by dropping the file.
pub const STATS_SCHEMA_VERSION: i64 = 1;

const STATS_DDL: &str = "\
CREATE TABLE snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
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
CREATE INDEX idx_snapshots_captured ON snapshots(captured_at);
";

/// One recorded analytics snapshot: when it was taken plus the full report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// RFC3339 capture time (UTC).
    pub captured_at: String,
    /// The full analytics report as recorded.
    pub report: StatsReport,
}

/// A handle to the durable `.clove/stats.db` history store.
#[derive(Debug)]
pub struct StatsStore {
    conn: Connection,
}

impl StatsStore {
    /// Open the stats store, initializing the schema on a brand-new file.
    ///
    /// Returns [`IndexError::SchemaMismatch`] on a version mismatch and
    /// [`IndexError::CorruptIndex`] on an unreadable file — neither is recovered
    /// by dropping the file (the history is durable, not a cache).
    pub fn open_or_create(path: &Utf8Path) -> Result<StatsStore, IndexError> {
        let conn = Connection::open(path).map_err(|e| {
            if is_corrupt(&e) {
                IndexError::CorruptIndex(e.to_string())
            } else {
                IndexError::SqliteError(e)
            }
        })?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )?;

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        match version {
            0 => {
                conn.execute_batch(STATS_DDL)?;
                conn.pragma_update(None, "user_version", STATS_SCHEMA_VERSION)?;
            }
            v if v == STATS_SCHEMA_VERSION => {}
            found => {
                return Err(IndexError::SchemaMismatch {
                    found,
                    expected: STATS_SCHEMA_VERSION,
                })
            }
        }
        Ok(StatsStore { conn })
    }

    /// Append one analytics snapshot stamped `captured_at`.
    pub fn record(
        &self,
        captured_at: DateTime<Utc>,
        report: &StatsReport,
    ) -> Result<(), IndexError> {
        let detail_json = serde_json::to_string(report).map_err(|e| {
            IndexError::CorruptIndex(format!("failed to serialize stats report: {e}"))
        })?;
        self.conn.execute(
            "INSERT INTO snapshots \
             (captured_at, total, open, in_progress, closed, ready, blocked, dangling, cycles, detail_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
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

    /// Read recorded snapshots, most recent first.
    ///
    /// `since` (an RFC3339 lower bound, inclusive) and `limit` are both optional;
    /// `None`/`0` mean unbounded.
    pub fn history(
        &self,
        since: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<StatsSnapshot>, IndexError> {
        let mut sql = String::from("SELECT captured_at, detail_json FROM snapshots");
        if since.is_some() {
            sql.push_str(" WHERE captured_at >= ?1");
        }
        sql.push_str(" ORDER BY captured_at DESC, id DESC");
        if let Some(n) = limit.filter(|&n| n > 0) {
            sql.push_str(&format!(" LIMIT {n}"));
        }

        let mut stmt = self.conn.prepare(&sql)?;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(String, String)> {
            Ok((row.get(0)?, row.get(1)?))
        };
        let rows = match since {
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
    pub fn count(&self) -> Result<usize, IndexError> {
        let n: i64 = self
            .conn
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
        let path = Utf8PathBuf::from_path_buf(dir.path().join("stats.db")).unwrap();
        (dir, path)
    }

    #[test]
    fn record_and_read_back() {
        let (_dir, path) = tmp_db();
        let store = StatsStore::open_or_create(&path).unwrap();
        assert_eq!(store.count().unwrap(), 0);

        let mut report = empty_report();
        report.total = 7;
        report.ready = 3;
        let t: DateTime<Utc> = "2026-06-01T00:00:00Z".parse().unwrap();
        store.record(t, &report).unwrap();

        let hist = store.history(None, None).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].report.total, 7);
        assert_eq!(hist[0].report.ready, 3);
    }

    #[test]
    fn history_orders_desc_and_filters_since() {
        let (_dir, path) = tmp_db();
        let store = StatsStore::open_or_create(&path).unwrap();

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
            store.record(ts.parse().unwrap(), &report).unwrap();
        }

        let all = store.history(None, None).unwrap();
        let times: Vec<&str> = all.iter().map(|s| s.captured_at.as_str()).collect();
        assert_eq!(times[0], "2026-06-03T00:00:00+00:00", "{times:?}");

        let since = store
            .history(Some("2026-06-02T00:00:00+00:00"), None)
            .unwrap();
        assert_eq!(since.len(), 2);

        let limited = store.history(None, Some(1)).unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].report.total, 2);
    }

    #[test]
    fn reopen_preserves_history() {
        let (_dir, path) = tmp_db();
        {
            let store = StatsStore::open_or_create(&path).unwrap();
            store.record(Utc::now(), &empty_report()).unwrap();
        }
        // A second open must NOT wipe the durable history (unlike the index cache).
        let store = StatsStore::open_or_create(&path).unwrap();
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn version_mismatch_is_reported_not_wiped() {
        let (_dir, path) = tmp_db();
        {
            let store = StatsStore::open_or_create(&path).unwrap();
            store.record(Utc::now(), &empty_report()).unwrap();
            store
                .conn
                .pragma_update(None, "user_version", 999_i64)
                .unwrap();
        }
        match StatsStore::open_or_create(&path) {
            Err(IndexError::SchemaMismatch { found, expected }) => {
                assert_eq!(found, 999);
                assert_eq!(expected, STATS_SCHEMA_VERSION);
            }
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
    }
}
