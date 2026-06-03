//! Store-scan benchmarks (DESIGN.md §13.1) — the M0 file-scan path behind
//! `clove ls` (`scan_frontmatter`, body-free) and `clove show`-bulk (`scan`,
//! full bodies), at 100 and 1,000 items.
//!
//! Run: `cargo bench -p clove-core --bench bench_scan`. Enforced gates live in
//! `tests/perf_gates.rs`.

use camino::Utf8PathBuf;
use clove_core::fixtures::write_fixtures;
use clove_core::ItemStore;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

/// Build a corpus of `n` items under a temp `.clove/issues/` and return a store.
fn corpus(n: usize) -> (tempfile::TempDir, ItemStore) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let issues = root.join(".clove").join("issues");
    write_fixtures(&issues, n, 0x5CA4_2026).unwrap();
    (tmp, ItemStore::new(root))
}

fn bench_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("scan");
    for &n in &[100usize, 1000] {
        let (_tmp, store) = corpus(n);

        // The `ls`/`ready` hot path: frontmatter only, no body allocation.
        group.bench_with_input(BenchmarkId::new("frontmatter", n), &store, |b, store| {
            b.iter(|| {
                let (items, _errs) = store.scan_frontmatter().unwrap();
                std::hint::black_box(items.len())
            })
        });

        // The full-load path (bodies materialized).
        group.bench_with_input(BenchmarkId::new("full", n), &store, |b, store| {
            b.iter(|| {
                let (items, _errs) = store.scan().unwrap();
                std::hint::black_box(items.len())
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_scan);
criterion_main!(benches);
