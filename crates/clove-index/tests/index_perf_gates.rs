//! M1 performance-gate tests: the DESIGN §6 / IMPLEMENTATION_PLAN "M1
//! Acceptance Gates" enforced as `#[test]`s (mirroring the M0 `perf_gates.rs`
//! pattern the project adopted). Wall-clock thresholds are asserted only for
//! optimized builds — run with `cargo test --release` to enforce the gate; debug
//! builds use a small corpus and skip the assertions (they would flake).
//!
//! Gates (warm index, 10k items):
//! - `reindex`                < 1000 ms (met, ~735 ms — incl. deferred covering-index build)
//! - `ls` (lean `query_list`)  <   8 ms (met, ~2.5–4.5 ms via the `idx_items_list` covering scan)
//! - `search` (FTS5, selective) < 20 ms (met, ~3 ms)
//! - staleness fast, 0 stale   <   5 ms (met, ~3 ms via `check_staleness_fast`)

use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use clove_core::fixtures::write_fixtures;
use clove_index::{reindex, Filter, Index, QueryMode};
use filetime::FileTime;
use tempfile::TempDir;

/// Items used for the gate. 10k in release (the documented bound); a small
/// corpus in debug so the default `cargo test` cycle stays fast (the thresholds
/// are not asserted there anyway). Override with `CLOVE_GATE_ITEMS`.
fn gate_items() -> usize {
    if let Ok(n) = std::env::var("CLOVE_GATE_ITEMS").map(|s| s.parse().unwrap_or(0)) {
        if n > 0 {
            return n;
        }
    }
    if cfg!(debug_assertions) {
        1_000
    } else {
        10_000
    }
}

fn best_of(iters: u32, mut op: impl FnMut()) -> Duration {
    let mut best = Duration::MAX;
    for _ in 0..iters {
        let start = Instant::now();
        op();
        best = best.min(start.elapsed());
    }
    best
}

fn assert_within(label: &str, elapsed: Duration, budget: Duration) {
    eprintln!("m1 perf gate {label}: {elapsed:?} (budget {budget:?})");
    if !cfg!(debug_assertions) {
        assert!(
            elapsed <= budget,
            "{label} took {elapsed:?}, over the {budget:?} budget (M1 acceptance gate)"
        );
    }
}

/// Backdate every item file and the issues dir so the staleness fast path does
/// not treat the just-written corpus as "recently modified".
fn backdate(issues: &camino::Utf8Path) {
    let past = FileTime::from_unix_time(1_600_000_000, 0);
    for entry in std::fs::read_dir(issues).unwrap() {
        filetime::set_file_mtime(entry.unwrap().path(), past).unwrap();
    }
    filetime::set_file_mtime(issues.as_std_path(), past).unwrap();
}

fn setup(n: usize) -> (TempDir, Utf8PathBuf, Utf8PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let issues = root.join(".clove").join("issues");
    write_fixtures(&issues, n, 0x6A7E_2026).unwrap();
    backdate(&issues);
    let db = root.join(".clove").join("index.db");
    (tmp, issues, db)
}

#[test]
fn m1_index_perf_gates() {
    let n = gate_items();
    let (_tmp, issues, db) = setup(n);

    // reindex gate (build the warm index in the process).
    let reindex_elapsed = best_of(3, || {
        reindex(&issues, &db).unwrap();
    });
    let index = Index::open(&db).unwrap();
    assert_eq!(
        index.item_count().unwrap(),
        n,
        "reindex must index every item"
    );
    assert_within("reindex", reindex_elapsed, Duration::from_millis(1000));

    // ls gate: the lean list projection (`query_list`) is what `clove ls` serves
    // from the index — only the columns the list renders, no per-row label parse.
    let mut ls_count = 0;
    let ls_elapsed = best_of(20, || {
        ls_count = index.query_list(&Filter::default()).unwrap().len();
    });
    assert_eq!(ls_count, n, "ls must return every item");
    // Gate: < 8 ms (tightened from 15 ms). With the `idx_items_list` covering
    // index the lean list is an index-only scan (~2.5–4.5 ms measured; per-row
    // step dropped from ~793 ns to ~116 ns once the second b-tree lookup was
    // removed). The 8 ms bound keeps ~2x headroom over the observed time so CI
    // catches a covering-scan regression (e.g. the index-only plan silently
    // reverting to a table scan) instead of hiding it under a loose budget.
    // See docs/M1_ACCEPTANCE_GATES.md.
    assert_within("ls_lean", ls_elapsed, Duration::from_millis(8));

    // ready gate: the lean projection in Ready mode.
    let ready_elapsed = best_of(20, || {
        let rows = index
            .query_list(&Filter {
                mode: QueryMode::Ready,
                ..Default::default()
            })
            .unwrap();
        std::hint::black_box(rows.len());
    });
    assert_within("ready_lean", ready_elapsed, Duration::from_millis(15));

    // FTS5 search gate. A real query is selective; "medium" matches the ~10% of
    // bodies in the medium-size class — the case the FTS index exists for.
    let mut hits = 0;
    let search_elapsed = best_of(20, || {
        hits = index.search("medium", None).unwrap().len();
    });
    assert!(
        hits > 0 && hits < n,
        "selective search must match a subset, got {hits}"
    );
    assert_within(
        "search_selective",
        search_elapsed,
        Duration::from_millis(20),
    );

    // Broad match ("benchmark" is in every body): informational. Shares ls's
    // row-materialization cost, so it exceeds 20ms at 10k — the same known gap.
    let broad = best_of(5, || {
        std::hint::black_box(index.search("benchmark", None).unwrap().len());
    });
    eprintln!("m1 perf gate search_broad_all_rows: {broad:?} (informational — see report)");

    // Staleness fast path, 0 stale: the O(readdir) `check_staleness_fast` (the
    // default the CLI uses) trusts dir mtime + count and skips per-file stats.
    let stale_elapsed = best_of(20, || {
        let report = index.check_staleness_fast(&issues).unwrap();
        assert!(report.is_clean(), "freshly indexed corpus must be clean");
    });
    assert_within(
        "staleness_clean_fast",
        stale_elapsed,
        Duration::from_millis(5),
    );

    // The deep path (`--deep`) stats every file; informational — it intentionally
    // exceeds the 5 ms budget at 10k (that is why it is opt-in).
    let deep = best_of(5, || {
        std::hint::black_box(index.check_staleness(&issues).unwrap().change_count());
    });
    eprintln!("m1 perf gate staleness_clean_deep: {deep:?} (opt-in --deep, informational)");
}
