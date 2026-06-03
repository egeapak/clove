//! beads-import throughput benchmark (T-M02, informational): plan + apply ~10k
//! generated beads issues into a fresh store.
//!
//! Run: `cargo bench -p clove-import --bench bench_import_beads`. No hard gate —
//! this tracks the cost of the beads parse/map/write pipeline over a 10k-issue
//! JSONL corpus.

use std::fmt::Write as _;

use camino::Utf8PathBuf;
use chrono::Utc;
use clove_core::ItemStore;
use clove_import::{BeadsImporter, ImportCtx, Importer};
use criterion::{criterion_group, criterion_main, Criterion};

/// Generate a deterministic beads-style `issues.jsonl` with `n` lines.
fn generate_jsonl(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        let status = ["open", "in_progress", "closed", "deferred"][i % 4];
        let issue_type = ["task", "bug", "feature", "docs"][i % 4];
        let _ = writeln!(
            out,
            r#"{{"id":"bd-{i}","title":"Issue number {i}","description":"Body for issue {i}.","status":"{status}","priority":{},"issue_type":"{issue_type}","owner":"user{}","labels":["area:core","perf"],"comment_count":0,"sprint":{}}}"#,
            i % 5,
            i % 7,
            i % 11
        );
    }
    out
}

fn bench_import_beads(c: &mut Criterion) {
    let src_tmp = tempfile::tempdir().unwrap();
    let src = Utf8PathBuf::from_path_buf(src_tmp.path().to_path_buf())
        .unwrap()
        .join("issues.jsonl");
    std::fs::write(&src, generate_jsonl(10_000)).unwrap();

    let mut group = c.benchmark_group("import_beads");
    group.bench_function("plan_apply_10k", |b| {
        b.iter(|| {
            // Fresh store per iteration so apply always creates (not skips).
            let store_tmp = tempfile::tempdir().unwrap();
            let root = Utf8PathBuf::from_path_buf(store_tmp.path().to_path_buf()).unwrap();
            std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
            let store = ItemStore::new(root);

            let importer = BeadsImporter::new("proj", Utc::now());
            let ctx = ImportCtx::new(&store, false).unwrap();
            let plan = importer.plan(&src, &ctx).unwrap();
            let report = importer.apply(plan, &store).unwrap();
            std::hint::black_box(report.created)
        })
    });
    group.finish();
}

criterion_group!(benches, bench_import_beads);
criterion_main!(benches);
