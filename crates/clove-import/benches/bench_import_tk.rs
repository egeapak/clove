//! tk-import throughput benchmark (T-M01, informational): plan + apply ~1k
//! generated tk tickets into a fresh store.
//!
//! Run: `cargo bench -p clove-import --bench bench_import_tk`. No hard gate —
//! this tracks the cost of the tk parse/map/write pipeline over a 1k-ticket
//! corpus.

use camino::Utf8PathBuf;
use chrono::Utc;
use clove_core::ItemStore;
use clove_import::{ImportCtx, Importer, TkImporter};
use criterion::{criterion_group, criterion_main, Criterion};

/// Write `n` deterministic tk-style tickets into `dir`.
fn write_tk_tickets(dir: &Utf8PathBuf, n: usize) {
    std::fs::create_dir_all(dir).unwrap();
    for i in 0..n {
        let status = ["open", "in_progress", "closed"][i % 3];
        let ticket_type = ["task", "bug", "feature", "docs"][i % 4];
        let body = format!(
            "---\nid: tk-{i}\nstatus: {status}\ntype: {ticket_type}\npriority: {}\ntags: [area:core, perf]\n---\n# Ticket number {i}\n\nBody text for ticket {i}.\n",
            i % 5
        );
        std::fs::write(dir.join(format!("ticket-{i}.md")), body).unwrap();
    }
}

fn bench_import_tk(c: &mut Criterion) {
    let src_tmp = tempfile::tempdir().unwrap();
    let src = Utf8PathBuf::from_path_buf(src_tmp.path().to_path_buf())
        .unwrap()
        .join(".tickets");
    write_tk_tickets(&src, 1_000);

    let mut group = c.benchmark_group("import_tk");
    group.bench_function("plan_apply_1k", |b| {
        b.iter(|| {
            // Fresh store per iteration so apply always creates (not skips).
            let store_tmp = tempfile::tempdir().unwrap();
            let root = Utf8PathBuf::from_path_buf(store_tmp.path().to_path_buf()).unwrap();
            std::fs::create_dir_all(root.join(".clove").join("issues")).unwrap();
            let store = ItemStore::new(root);

            let importer = TkImporter::new("proj", Utc::now());
            let ctx = ImportCtx::new(&store, false).unwrap();
            let plan = importer.plan(&src, &ctx).unwrap();
            let report = importer.apply(plan, &store).unwrap();
            std::hint::black_box(report.created)
        })
    });
    group.finish();
}

criterion_group!(benches, bench_import_tk);
criterion_main!(benches);
