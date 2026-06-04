//! Periodic work-item analytics snapshots (M4 daemon auto-snapshot).
//!
//! A running daemon records a [`clove_core::StatsReport`] into the index's durable
//! `snapshots` history on a configurable interval (`[daemon] stats_snapshot_min`,
//! default 60), so `clove stats --history` shows a trend without anyone having to
//! run `clove stats --snapshot` by hand. The snapshot is computed from a full file
//! scan + graph build — identical to the `clove stats` CLI path — so a daemon
//! snapshot and a manual one are byte-for-byte the same. This is off the hot path
//! (it fires at most a few times per active daemon session).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use camino::Utf8Path;
use chrono::Utc;
use clove_core::{compute_stats, GraphStore, ItemStore, StatsOptions};
use clove_index::Index;

/// Compute one analytics snapshot from the files under `repo_root` and record it
/// into the index's history. Returns `true` if a snapshot was recorded. Best
/// effort: a scan/record failure is swallowed (a missed analytics point is never
/// worth crashing the daemon).
pub fn snapshot_once(repo_root: &Utf8Path, index: &Arc<Mutex<Index>>) -> bool {
    let store = ItemStore::new(repo_root.to_owned());
    let Ok((frontmatters, _errors)) = store.scan_frontmatter() else {
        return false;
    };
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let now = Utc::now();
    let report = compute_stats(&frontmatters, &graph, now, StatsOptions::default());
    match index.lock() {
        Ok(idx) => idx.record_snapshot(now, &report).is_ok(),
        Err(_) => false,
    }
}

/// Resolve the auto-snapshot interval. `stats_snapshot_min == 0` disables it
/// (returns `None`). `CLOVED_STATS_SNAPSHOT_MS` overrides it (sub-minute values
/// for tests); `0` there also disables.
pub fn snapshot_interval(stats_snapshot_min: u64) -> Option<Duration> {
    if let Ok(ms) = std::env::var("CLOVED_STATS_SNAPSHOT_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            return (ms > 0).then(|| Duration::from_millis(ms));
        }
    }
    (stats_snapshot_min > 0).then(|| Duration::from_secs(stats_snapshot_min * 60))
}

/// Record an analytics snapshot every `interval`, until cancelled. Never resolves
/// when `interval` is `None` (auto-snapshot disabled). The first tick is consumed
/// so no snapshot is taken at t=0 (startup already has the prior session's
/// history). The scan/record runs on a blocking thread so it never stalls the
/// async runtime.
pub async fn snapshot_loop(
    repo_root: camino::Utf8PathBuf,
    index: Arc<Mutex<Index>>,
    interval: Option<Duration>,
) {
    let Some(interval) = interval else {
        std::future::pending::<()>().await;
        return;
    };
    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await; // the immediate first tick — skip the t=0 snapshot
    loop {
        ticker.tick().await;
        let repo_root = repo_root.clone();
        let index = Arc::clone(&index);
        let _ = tokio::task::spawn_blocking(move || snapshot_once(&repo_root, &index)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn write_item(issues: &Utf8Path, id: &str) {
        std::fs::write(
            issues.join(format!("{id}.md")),
            format!(
                "---\nschema: 1\nid: {id}\ntitle: {id}\nstatus: open\ntype: feature\n\
                 priority: 2\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n---\nbody\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn snapshot_once_records_a_history_row() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let issues = root.join(".clove/issues");
        std::fs::create_dir_all(&issues).unwrap();
        write_item(&issues, "proj-AAAAAAAA");
        write_item(&issues, "proj-BBBBBBBB");

        let db = root.join(".clove/index.db");
        clove_index::reindex(&issues, &db).unwrap();
        let index = Arc::new(Mutex::new(Index::open(&db).unwrap()));

        assert!(snapshot_once(&root, &index));
        assert!(snapshot_once(&root, &index));

        let idx = index.lock().unwrap();
        assert_eq!(idx.snapshot_count().unwrap(), 2);
        let hist = idx.snapshot_history(None, Some(1)).unwrap();
        assert_eq!(hist[0].report.total, 2, "snapshot reflects the two items");
    }

    #[test]
    fn interval_zero_disables() {
        // With the env override unset, 0 minutes disables.
        if std::env::var("CLOVED_STATS_SNAPSHOT_MS").is_err() {
            assert!(snapshot_interval(0).is_none());
            assert_eq!(snapshot_interval(60), Some(Duration::from_secs(3600)));
        }
    }
}
