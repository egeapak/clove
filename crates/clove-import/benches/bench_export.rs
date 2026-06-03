//! Export-throughput benchmark (T-M04, informational): serialize a 10k-item
//! fixture to JSONL via [`clove_import::export::export_jsonl`].
//!
//! Run: `cargo bench -p clove-import --bench bench_export`. No hard gate — this
//! tracks the NDJSON writer's cost over the canonical 10k-item corpus.

use camino::Utf8PathBuf;
use clove_core::fixtures::write_fixtures;
use clove_core::ItemStore;
use clove_import::export::export_jsonl;
use criterion::{criterion_group, criterion_main, Criterion};
use serde_json::Value;

/// Build a 10k-item corpus and shape it into the JSON item objects the export
/// writer consumes (frontmatter serialization — the same field set `clove
/// export` emits, minus the body/graph augmentation, which is out of scope for
/// the writer benchmark).
fn corpus(n: usize) -> (tempfile::TempDir, Vec<Value>) {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let issues = root.join(".clove").join("issues");
    write_fixtures(&issues, n, 0x5CA4_2026).unwrap();
    let store = ItemStore::new(root);
    let (items, _errs) = store.scan().unwrap();
    let values: Vec<Value> = items
        .iter()
        .map(|i| serde_json::to_value(&i.frontmatter).unwrap())
        .collect();
    (tmp, values)
}

fn bench_export(c: &mut Criterion) {
    let (_tmp, items) = corpus(10_000);
    let mut group = c.benchmark_group("export");
    group.bench_function("jsonl_10k", |b| {
        b.iter(|| {
            let mut sink = Vec::with_capacity(2 * 1024 * 1024);
            export_jsonl(&mut sink, &items).unwrap();
            std::hint::black_box(sink.len())
        })
    });
    group.finish();
}

criterion_group!(benches, bench_export);
criterion_main!(benches);
