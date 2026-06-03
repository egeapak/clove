//! Parser micro-benchmarks (DESIGN.md §13.1): full item parse vs. the
//! body-free frontmatter fast path used by `ls`/`ready`.
//!
//! Run: `cargo bench -p clove-core --bench bench_parse`. The enforced timing
//! gates live in `tests/perf_gates.rs`; criterion here is for tracking trends.

use camino::Utf8PathBuf;
use clove_core::fixtures::write_fixtures;
use clove_core::{parse_frontmatter_file, parse_item_file};
use criterion::{criterion_group, criterion_main, Criterion};

/// Generate a small corpus and return the path of one representative item file.
fn sample_item() -> (tempfile::TempDir, Utf8PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let ids = write_fixtures(&dir, 64, 0xB1A5_2026).unwrap();
    // Pick an item far enough in to likely carry deps/labels/body variety.
    let path = dir.join(format!("{}.md", ids[40].as_str()));
    (tmp, path)
}

fn bench_parse(c: &mut Criterion) {
    let (_tmp, path) = sample_item();

    c.bench_function("parse_item_file_full", |b| {
        b.iter(|| parse_item_file(std::hint::black_box(&path)).unwrap())
    });

    c.bench_function("parse_frontmatter_file_lazy", |b| {
        b.iter(|| parse_frontmatter_file(std::hint::black_box(&path)).unwrap())
    });
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
