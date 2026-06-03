//! Performance-gate tests (T-X01 acceptance criterion): the DESIGN.md §13.1
//! file-scan targets enforced as `#[test]`s with `std::time::Instant` (not
//! criterion), so CI fails on a regression.
//!
//! The workload (and its correctness assertions) always runs. The wall-clock
//! thresholds are only asserted for optimized builds — debug builds are several
//! times slower and would flake — so run the gate with `cargo test --release`.
//! Each measurement takes the minimum of several iterations to denoise shared CI
//! runners while still catching real regressions against the documented bound.

use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use clove_core::fixtures::write_fixtures;
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

/// Assert `elapsed <= budget`, but only in optimized builds.
fn assert_within(label: &str, elapsed: Duration, budget: Duration) {
    eprintln!("perf gate {label}: {elapsed:?} (budget {budget:?})");
    if !cfg!(debug_assertions) {
        assert!(
            elapsed <= budget,
            "{label} took {elapsed:?}, over the {budget:?} budget (DESIGN §13.1)"
        );
    }
}

#[test]
fn ls_1000_items_scan_under_budget() {
    // `clove ls` hot path: body-free frontmatter scan of 1,000 items. §13.1
    // target: < 50 ms cold.
    let (_tmp, store) = corpus(1000);
    let mut count = 0;
    let elapsed = best_of(5, || {
        let (items, errs) = store.scan_frontmatter().unwrap();
        assert!(errs.is_empty(), "fixture corpus must parse cleanly");
        count = items.len();
    });
    assert_eq!(count, 1000, "ls scan must see every item");
    assert_within("ls_1000_scan", elapsed, Duration::from_millis(50));
}

#[test]
fn ready_1000_items_under_budget() {
    // `clove ready` path: scan + graph build + ready query over 1,000 items.
    // §13.1 target: < 80 ms cold.
    let (_tmp, store) = corpus(1000);
    let mut ready_count = 0;
    let elapsed = best_of(5, || {
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
    assert_within("ready_1000", elapsed, Duration::from_millis(80));
}

#[test]
fn ls_100_items_scan_under_budget() {
    // §13.1 target: `clove ls` 100 items < 10 ms cold.
    let (_tmp, store) = corpus(100);
    let elapsed = best_of(8, || {
        let (items, _errs) = store.scan_frontmatter().unwrap();
        std::hint::black_box(items.len());
    });
    assert_within("ls_100_scan", elapsed, Duration::from_millis(10));
}
