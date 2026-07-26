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

/// Re-spell every `captured_at` that is not already canonical.
///
/// Called from [`Index::record_snapshot`] — i.e. on the next *mutation* of the
/// history, never on a read. There is no flag day and no `clove migrate`: a repo
/// that never snapshots again keeps its old rows, and reads canonicalize on the
/// way out regardless. Cheap by construction (one row per `clove stats
/// --snapshot`), and a no-op once the table is canonical.
fn canonicalize_captured_at(conn: &Connection) -> rusqlite::Result<()> {
    let stale: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, captured_at FROM snapshots")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(id, raw)| {
                let canonical = clove_types::canonicalize_rfc3339(&raw);
                (canonical != raw).then_some((id, canonical))
            })
            .collect()
    };
    for (id, canonical) in stale {
        conn.execute(
            "UPDATE snapshots SET captured_at = ?1 WHERE id = ?2",
            params![canonical, id],
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
    ///
    /// The timestamp is written in the one canonical spelling
    /// ([`clove_types::canonical_rfc3339`]). Rows left by an older clove are
    /// re-spelled on the way past — the roadmap's "rewrite on the next
    /// mutation", which for an append-only history table is the next append.
    /// (The rewrite and the append are separate autocommit statements, not one
    /// transaction; the rewrite is idempotent, so a crash between them costs
    /// nothing.)
    pub fn record_snapshot(
        &self,
        captured_at: DateTime<Utc>,
        report: &StatsReport,
    ) -> Result<(), IndexError> {
        ensure_table(self.conn())?;
        canonicalize_captured_at(self.conn())?;
        let detail_json = serde_json::to_string(report).map_err(|e| {
            IndexError::CorruptIndex(format!("failed to serialize stats report: {e}"))
        })?;
        self.conn().execute(
            "INSERT INTO snapshots \
             (captured_at, total, open, in_progress, closed, ready, blocked, dangling, cycles, detail_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                clove_types::canonical_rfc3339(captured_at),
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
    /// Both the `since` comparison and the ordering run over the **parsed
    /// instant**, in Rust, not over the stored string.
    ///
    /// An earlier version compared `substr(captured_at, 1, 19)` in SQL, which is
    /// the *local wallclock* prefix: a legacy row spelled `…T10:00:00+02:00`
    /// (08:00Z) sorted as though it were 10:00, so history came back visibly
    /// unsorted, `--since` selected too many rows, and `--limit` dropped the
    /// wrong ones. The read path canonicalized the returned *value* while
    /// ordering on the raw prefix, so the output contradicted itself rather than
    /// failing loudly. Only clove's own writes are guaranteed UTC; this table is
    /// carried verbatim across every reindex and schema bump, so it can hold
    /// anything an older clove or a foreign writer left.
    ///
    /// The table holds one row per `clove stats --snapshot`, so reading it whole
    /// and ordering in memory is cheap and unconditionally correct. Unparseable
    /// rows sort last, deterministically, rather than being dropped.
    ///
    /// The returned `captured_at` is always canonical, whatever the row holds.
    pub fn snapshot_history(
        &self,
        since: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<StatsSnapshot>, IndexError> {
        ensure_table(self.conn())?;
        let since_instant = since.and_then(clove_types::parse_rfc3339);

        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, captured_at, detail_json FROM snapshots ORDER BY id DESC")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut parsed = Vec::new();
        for row in rows {
            let (id, captured_at, detail_json) = row?;
            let report: StatsReport = serde_json::from_str(&detail_json).map_err(|e| {
                IndexError::CorruptIndex(format!("corrupt snapshot at {captured_at}: {e}"))
            })?;
            let instant = clove_types::parse_rfc3339(&captured_at);
            if let (Some(bound), Some(at)) = (since_instant, instant) {
                if at < bound {
                    continue;
                }
            }
            parsed.push((instant, id, captured_at, report));
        }

        // Newest first, by instant. `None` (unparseable) sorts last rather than
        // being dropped; `id` breaks ties so the order is total.
        parsed.sort_by(|a, b| match (a.0, b.0) {
            (Some(x), Some(y)) => y.cmp(&x).then_with(|| b.1.cmp(&a.1)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => b.1.cmp(&a.1),
        });
        if let Some(n) = limit.filter(|&n| n > 0) {
            parsed.truncate(n);
        }

        Ok(parsed
            .into_iter()
            .map(|(_, _, captured_at, report)| StatsSnapshot {
                captured_at: clove_types::canonicalize_rfc3339(&captured_at),
                report,
            })
            .collect())
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
        assert_eq!(times[0], "2026-06-03T00:00:00Z", "{times:?}");

        let since = index
            .snapshot_history(Some("2026-06-02T00:00:00+00:00"), None)
            .unwrap();
        assert_eq!(since.len(), 2);

        // A `Z`-suffixed bound is equivalent to the `+00:00` one and must include
        // the boundary snapshot (regression: naive lexicographic compare would
        // drop it because '+' < 'Z').
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

    /// Insert a snapshot row with a **verbatim** `captured_at` string, bypassing
    /// `record_snapshot`. Every legacy-spelling test below has to write its rows
    /// this way: going through the writer would canonicalize the input, and a
    /// fixture that cannot hold a non-canonical value cannot detect a missing
    /// canonicalization.
    fn insert_verbatim(index: &Index, captured_at: &str, total: i64) {
        let detail = serde_json::to_string(&{
            let mut r = empty_report();
            r.total = total as u64;
            r
        })
        .unwrap();
        index
            .conn()
            .execute(
                "INSERT INTO snapshots (captured_at, total, open, in_progress, closed, \
                 ready, blocked, dangling, cycles, detail_json) \
                 VALUES (?1, ?2, 0, 0, 0, 0, 0, 0, 0, ?3)",
                params![captured_at, total, detail],
            )
            .unwrap();
    }

    #[test]
    fn recorded_captured_at_is_canonical() {
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();
        // A wall-clock instant with nanoseconds — what `stats --snapshot` used to
        // store verbatim (`…T10:00:00.904816670+00:00`).
        let t = DateTime::parse_from_rfc3339("2026-06-01T10:00:00.904816670+00:00")
            .unwrap()
            .with_timezone(&Utc);
        index.record_snapshot(t, &empty_report()).unwrap();

        let raw: String = index
            .conn()
            .query_row("SELECT captured_at FROM snapshots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            raw, "2026-06-01T10:00:00Z",
            "the stored string itself must be canonical"
        );
    }

    #[test]
    fn legacy_spellings_read_back_canonical() {
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();
        ensure_table(index.conn()).unwrap();
        insert_verbatim(&index, "2026-06-01T10:00:00.904816670+00:00", 1);

        let hist = index.snapshot_history(None, None).unwrap();
        assert_eq!(
            hist[0].captured_at, "2026-06-01T10:00:00Z",
            "a row written by an older clove must still read back canonical"
        );
    }

    #[test]
    fn history_orders_mixed_spellings_by_instant() {
        // The ordering hazard §3 names: `captured_at` is ordered as TEXT, and the
        // durable snapshots table survives every rebuild, so rows in the old and
        // new spellings coexist.
        //
        // Rows 0-2 are one second apart in three different spellings. Rows 3-4
        // are the case that actually discriminates: two rows *inside the same
        // second*, where the suffix byte is what a raw `ORDER BY captured_at`
        // would compare ('+' < '.' < 'Z'), and where canonical semantics say the
        // two are the same instant and the `id DESC` tiebreak decides. Written
        // canonical-first so the two orders disagree.
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();
        ensure_table(index.conn()).unwrap();
        insert_verbatim(&index, "2026-06-01T10:00:02Z", 2); // canonical
        insert_verbatim(&index, "2026-06-01T10:00:00.904816670+00:00", 0); // legacy
        insert_verbatim(&index, "2026-06-01T10:00:01+00:00", 1); // legacy
        insert_verbatim(&index, "2026-06-01T10:00:03Z", 3); // canonical
        insert_verbatim(&index, "2026-06-01T10:00:03.500000000+00:00", 4); // legacy

        let totals: Vec<u64> = index
            .snapshot_history(None, None)
            .unwrap()
            .iter()
            .map(|s| s.report.total)
            .collect();
        assert_eq!(totals, vec![4, 3, 2, 1, 0], "newest first, by instant");
    }

    /// A non-UTC offset sorts and filters by its *instant*, not its wallclock.
    ///
    /// Ordering used to compare `substr(captured_at, 1, 19)` — which is the
    /// local wallclock prefix, not a UTC one. A row spelled `…T10:00:00+02:00`
    /// is 08:00Z, but sorted as though it were 10:00: history came back visibly
    /// unsorted, `--since` selected too many rows, and `--limit` dropped the
    /// wrong ones. Worse, the returned *value* was canonicalized while the
    /// ordering was not, so the output contradicted itself instead of failing.
    ///
    /// Only clove's own writes are guaranteed UTC; this table is carried
    /// verbatim across every reindex and schema bump, so it can hold whatever an
    /// older clove or a foreign writer left.
    #[test]
    fn a_non_utc_offset_orders_by_instant_not_wallclock() {
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();
        ensure_table(index.conn()).unwrap();
        // 08:00Z, but spelled with a +02:00 offset: its wallclock prefix (10:00)
        // sorts it above rows that are genuinely later.
        insert_verbatim(&index, "2026-06-01T10:00:00+02:00", 0); // = 08:00Z
        insert_verbatim(&index, "2026-06-01T08:30:00Z", 1);
        insert_verbatim(&index, "2026-06-01T09:00:00Z", 2);

        let totals: Vec<u64> = index
            .snapshot_history(None, None)
            .unwrap()
            .iter()
            .map(|s| s.report.total)
            .collect();
        assert_eq!(totals, vec![2, 1, 0], "newest first, by instant");

        // `--since` uses the instant too: 08:30Z excludes the 08:00Z row.
        let since: Vec<u64> = index
            .snapshot_history(Some("2026-06-01T08:30:00Z"), None)
            .unwrap()
            .iter()
            .map(|s| s.report.total)
            .collect();
        assert_eq!(
            since,
            vec![2, 1],
            "the +02:00 row is 08:00Z, below the bound"
        );

        // ...and `limit` keeps the genuinely-newest, not the wallclock-newest.
        let newest: Vec<u64> = index
            .snapshot_history(None, Some(1))
            .unwrap()
            .iter()
            .map(|s| s.report.total)
            .collect();
        assert_eq!(newest, vec![2]);
    }

    #[test]
    fn since_bound_spans_spellings_in_both_directions() {
        // The `since` bound and the stored rows can each be in either spelling,
        // and every combination has to agree on the boundary row (inclusive).
        //
        // The boundary row is deliberately a *legacy*-spelled one: that is the
        // combination a raw `captured_at >= ?` drops, because both `'+'` and
        // `'.'` sort below the `'Z'` of a canonicalized bound — a snapshot
        // silently missing from `stats --history --since`.
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();
        ensure_table(index.conn()).unwrap();
        insert_verbatim(&index, "2026-06-01T00:00:00+00:00", 0); // before
        insert_verbatim(&index, "2026-06-02T00:00:00.904816670+00:00", 1); // boundary, legacy
        insert_verbatim(&index, "2026-06-03T00:00:00Z", 2); // after

        for bound in [
            "2026-06-02T00:00:00Z",
            "2026-06-02T00:00:00+00:00",
            "2026-06-02T00:00:00.000Z",
            "2026-06-02T02:00:00+02:00",
        ] {
            let got = index.snapshot_history(Some(bound), None).unwrap();
            assert_eq!(
                got.len(),
                2,
                "`{bound}` must select the boundary row and everything after it"
            );
        }
    }

    #[test]
    fn recording_rewrites_legacy_rows() {
        // Roadmap §3's "rewrite on the next mutation" — no flag day, no `clove
        // migrate`. For an append-only history table the next mutation is the
        // next `stats --snapshot`.
        let (_dir, path) = tmp_db();
        let index = Index::open(&path).unwrap();
        ensure_table(index.conn()).unwrap();
        insert_verbatim(&index, "2026-06-01T10:00:00.904816670+00:00", 0);
        insert_verbatim(&index, "2026-06-01T10:00:01+00:00", 1);

        index
            .record_snapshot("2026-06-01T10:00:02Z".parse().unwrap(), &empty_report())
            .unwrap();

        let mut stmt = index
            .conn()
            .prepare("SELECT captured_at FROM snapshots ORDER BY id")
            .unwrap();
        let stored: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            stored,
            vec![
                "2026-06-01T10:00:00Z",
                "2026-06-01T10:00:01Z",
                "2026-06-01T10:00:02Z"
            ],
            "recording a snapshot re-spells the rows already there"
        );
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
    fn schema_mismatch_rebuild_preserves_history() {
        let (_dir, path) = tmp_db();
        {
            let index = Index::open(&path).unwrap();
            index.record_snapshot(Utc::now(), &empty_report()).unwrap();
            // Simulate a future/incompatible *cache* schema version: open_or_create
            // must drop-and-rebuild the cache tables but carry the durable history
            // across (db.rs preserves it via preserve_from/insert_raw).
            index
                .conn()
                .pragma_update(None, "user_version", 999_i64)
                .unwrap();
        }
        let index = Index::open_or_create(&path).unwrap();
        assert_eq!(
            index.snapshot_count().unwrap(),
            1,
            "history must survive a schema-mismatch rebuild"
        );
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
