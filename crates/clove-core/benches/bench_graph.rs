//! Dependency-graph benchmarks (DESIGN.md §13.1) — the engine behind
//! `clove ready`/`blocked`/`dep cycle`/`dep tree`, on a 1,000-item corpus.
//!
//! Run: `cargo bench -p clove-core --bench bench_graph`. Enforced gates live in
//! `tests/perf_gates.rs`.

use camino::Utf8PathBuf;
use clove_core::fixtures::write_fixtures;
use clove_core::{GraphStore, ItemStore};
use clove_types::ItemFrontmatter;
use criterion::{criterion_group, criterion_main, Criterion};

/// Generate a 1,000-item corpus and return its parsed frontmatters.
fn frontmatters(n: usize) -> Vec<ItemFrontmatter> {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let issues = root.join(".clove").join("issues");
    write_fixtures(&issues, n, 0x6DA6_2026).unwrap();
    let store = ItemStore::new(root);
    store.scan_frontmatter().unwrap().0
}

fn bench_graph(c: &mut Criterion) {
    let fms = frontmatters(1000);

    c.bench_function("graph_build_1000", |b| {
        b.iter(|| {
            let (graph, _dangling) = GraphStore::build(std::hint::black_box(&fms));
            std::hint::black_box(graph.len())
        })
    });

    let (graph, _dangling) = GraphStore::build(&fms);

    c.bench_function("graph_ready_1000", |b| {
        b.iter(|| std::hint::black_box(graph.ready_items().len()))
    });

    c.bench_function("graph_blocked_1000", |b| {
        b.iter(|| std::hint::black_box(graph.blocked_items().len()))
    });

    c.bench_function("graph_all_cycles_1000", |b| {
        b.iter(|| std::hint::black_box(graph.all_cycles().len()))
    });

    c.bench_function("graph_topo_ranks_1000", |b| {
        b.iter(|| std::hint::black_box(graph.topological_ranks().len()))
    });
}

criterion_group!(benches, bench_graph);
criterion_main!(benches);
