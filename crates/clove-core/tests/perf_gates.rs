//! Performance-gate tests (T-X01 acceptance criterion): the DESIGN.md §13.1
//! file-scan targets enforced as `#[test]`s with `std::time::Instant` (not
//! criterion), so CI fails on a regression.
//!
//! The workload (and its correctness assertions) always runs. The wall-clock
//! thresholds are only asserted for optimized builds — debug builds are several
//! times slower and would flake — so run the gate with `cargo test --release`.
//! Each gate takes the minimum of several iterations, and measures up to
//! `GATE_ROUNDS` rounds: a gate fails only when every round is over its bound,
//! so a burst of load on a shared CI runner does not fail it, but a real
//! regression does.

use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use clove_core::fixtures::{perf_gate, write_fixtures, GATE_PAUSE, GATE_ROUNDS};
use clove_core::{GraphStore, ItemStore};
use tempfile::TempDir;

/// Build an `n`-item corpus under a temp `.clove/issues/` and return its store.
fn corpus(n: usize) -> (TempDir, ItemStore) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let issues = root.join(".clove").join("issues");
    write_fixtures(&issues, n, 0x9A7E_2026).unwrap();
    (tmp, ItemStore::new(root))
}

/// Run `op` `iters` times and return the fastest observed duration.
fn best_of(iters: u32, mut op: impl FnMut()) -> Duration {
    let mut best = Duration::MAX;
    for _ in 0..iters {
        let start = Instant::now();
        op();
        best = best.min(start.elapsed());
    }
    best
}

/// Measure `op` with [`perf_gate`]: fail only when every round is over
/// `budget`, and only in optimized builds. A debug build runs one round and
/// asserts nothing.
fn gate(label: &str, iters: u32, budget: Duration, op: impl FnMut()) {
    let rounds = if cfg!(debug_assertions) {
        1
    } else {
        GATE_ROUNDS
    };
    let run = perf_gate(rounds, iters, budget, GATE_PAUSE, op);
    eprintln!(
        "perf gate {label}: {:?} (budget {budget:?}, rounds {:?})",
        run.best(),
        run.rounds
    );
    if !cfg!(debug_assertions) {
        assert!(
            run.passed(),
            "{label}: every round was over the {budget:?} budget: {:?} (DESIGN §13.1)",
            run.rounds
        );
    }
}

#[test]
fn ls_1000_items_scan_under_budget() {
    // `clove ls` hot path: body-free frontmatter scan of 1,000 items. §13.1
    // target: < 50 ms cold.
    let (_tmp, store) = corpus(1000);
    let mut count = 0;
    gate("ls_1000_scan", 5, Duration::from_millis(50), || {
        let (items, errs) = store.scan_frontmatter().unwrap();
        assert!(errs.is_empty(), "fixture corpus must parse cleanly");
        count = items.len();
    });
    assert_eq!(count, 1000, "ls scan must see every item");
}

#[test]
fn ready_1000_items_under_budget() {
    // `clove ready` path: scan + graph build + ready query over 1,000 items.
    // §13.1 target: < 80 ms cold.
    let (_tmp, store) = corpus(1000);
    let mut ready_count = 0;
    gate("ready_1000", 5, Duration::from_millis(80), || {
        let (frontmatters, errs) = store.scan_frontmatter().unwrap();
        assert!(errs.is_empty());
        let (graph, _dangling) = GraphStore::build(&frontmatters);
        ready_count = graph.ready_items().len();
    });
    // The fixture mixes open/closed/in_progress with deps, so some — but not
    // all — items are ready.
    assert!(
        ready_count > 0 && ready_count < 1000,
        "ready set should be a non-trivial subset, got {ready_count}"
    );
}

#[test]
fn ls_100_items_scan_under_budget() {
    // §13.1 target: `clove ls` 100 items < 10 ms cold.
    let (_tmp, store) = corpus(100);
    gate("ls_100_scan", 8, Duration::from_millis(10), || {
        let (items, _errs) = store.scan_frontmatter().unwrap();
        std::hint::black_box(items.len());
    });
}

#[test]
fn show_one_item_is_independent_of_store_size() {
    // `ops::show` derives `ready`/`blocked_by` from the item's own dependency
    // closure rather than a whole-store scan + graph build. The property that
    // matters is not a wall-clock number but the *shape*: showing one item must
    // not get slower as the store grows.
    //
    // Guarding the shape rather than an absolute budget keeps this meaningful on
    // a slow or loaded CI box, where any fixed millisecond figure is noise.
    let (_small, small) = corpus(200);
    let (_large, large) = corpus(5000);

    let small_ids = small.scan_frontmatter().unwrap().0;
    let large_ids = large.scan_frontmatter().unwrap().0;

    // Warm the page cache so this measures work, not first-read I/O.
    let _ = clove_core::ops::show(&small, &small_ids[0].id).unwrap();
    let _ = clove_core::ops::show(&large, &large_ids[0].id).unwrap();

    let small_t = best_of(10, || {
        clove_core::ops::show(&small, &small_ids[0].id).unwrap();
    });
    let large_t = best_of(10, || {
        clove_core::ops::show(&large, &large_ids[0].id).unwrap();
    });

    eprintln!("perf gate show: 200 items {small_t:?}, 5000 items {large_t:?}");
    if !cfg!(debug_assertions) {
        // A whole-store implementation would be ~25x slower on the 25x corpus.
        // Allow a wide margin for timer noise on a small absolute duration and
        // still fail loudly if the scan ever comes back.
        assert!(
            large_t < small_t * 8 + Duration::from_millis(2),
            "show on 5000 items ({large_t:?}) scaled with store size vs 200 items \
             ({small_t:?}) — the whole-store scan has returned"
        );
    }
}
