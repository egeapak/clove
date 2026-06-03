//! Criterion benchmarks for the M1 acceptance gates (DESIGN §6, plan M1 gates):
//! reindex < 1000ms, warm `ls` < 10ms, staleness (0 stale) < 5ms — all at 10k
//! items. Run with `cargo bench -p clove-index`; set `CLOVE_BENCH_ITEMS` to vary
//! the corpus size.

use camino::Utf8PathBuf;
use clove_index::{reindex, Filter, Index, QueryMode};
use criterion::{criterion_group, criterion_main, Criterion};

fn item_count() -> usize {
    std::env::var("CLOVE_BENCH_ITEMS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000)
}

fn id_for(i: usize) -> String {
    const ALPH: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut n = i as u64;
    let mut buf = [b'0'; 8];
    let mut p = 8;
    while n > 0 && p > 0 {
        p -= 1;
        buf[p] = ALPH[(n % 32) as usize];
        n /= 32;
    }
    format!("proj-{}", String::from_utf8(buf.to_vec()).unwrap())
}

/// Build a temp repo of `n` items. ~10% reference an earlier item as a hard dep
/// to exercise the topological sort and the ready query's NOT EXISTS subquery.
fn build_corpus(n: usize) -> (tempfile::TempDir, Utf8PathBuf, Utf8PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    let issues = root.join(".clove/issues");
    std::fs::create_dir_all(&issues).unwrap();
    for i in 0..n {
        let id = id_for(i);
        let priority = i % 5;
        let dep = if i > 0 && i % 10 == 0 {
            format!("deps:\n  - {}\n", id_for(i - 1))
        } else {
            String::new()
        };
        let body = format!(
            "---\nschema: 1\nid: {id}\ntitle: Item {i}\nstatus: open\ntype: feature\n\
             priority: {priority}\ncreated: 2026-06-02T10:00:00Z\nupdated: 2026-06-02T10:00:00Z\n\
             {dep}---\nThe quick brown fox jumps over item {i} with keyword{i}.\n"
        );
        std::fs::write(issues.join(format!("{id}.md")), body).unwrap();
    }
    // Backdate so the staleness "recent file" guard does not force hashing.
    let past = filetime::FileTime::from_unix_time(1_600_000_000, 0);
    for entry in std::fs::read_dir(&issues).unwrap() {
        filetime::set_file_mtime(entry.unwrap().path(), past).unwrap();
    }
    filetime::set_file_mtime(issues.as_std_path(), past).unwrap();
    let db = root.join(".clove/index.db");
    (dir, issues, db)
}

fn bench_index(c: &mut Criterion) {
    let n = item_count();
    let (_dir, issues, db) = build_corpus(n);

    let mut reindex_group = c.benchmark_group("reindex");
    reindex_group.sample_size(10);
    reindex_group.bench_function(format!("reindex_{n}"), |b| {
        b.iter(|| {
            reindex(&issues, &db).unwrap();
        });
    });
    reindex_group.finish();

    // Warm index for the read benchmarks.
    reindex(&issues, &db).unwrap();
    let index = Index::open(&db).unwrap();

    c.bench_function(&format!("ls_{n}"), |b| {
        b.iter(|| {
            let rows = index.query_items(&Filter::default()).unwrap();
            criterion::black_box(rows.len());
        });
    });
    c.bench_function(&format!("ready_{n}"), |b| {
        b.iter(|| {
            let rows = index
                .query_items(&Filter {
                    mode: QueryMode::Ready,
                    ..Default::default()
                })
                .unwrap();
            criterion::black_box(rows.len());
        });
    });
    c.bench_function(&format!("staleness_clean_{n}"), |b| {
        b.iter(|| {
            let report = index.check_staleness(&issues).unwrap();
            criterion::black_box(report.change_count());
        });
    });
    // M1 gate: FTS5 `clove search` at 10k items should be < 20ms. The corpus
    // bodies all contain "keyword<i>"; "fox" appears in every body, so this is a
    // realistically broad match.
    c.bench_function(&format!("search_{n}"), |b| {
        b.iter(|| {
            let rows = index.search("fox", None).unwrap();
            criterion::black_box(rows.len());
        });
    });
}

criterion_group!(benches, bench_index);
criterion_main!(benches);
