//! Detailed timing breakdown of the `ls` index read at 10k items: where the
//! ~11 ms goes. Opens its own connection to a reindexed corpus and times, with
//! increasing work per row, the same statement:
//!
//!   prepare → step-only → +read int → +decode lean (SmolStr) → +decode lean
//!   (String) → +decode full 15-col row (the old `query_items` path).
//!
//! Differences isolate: SQL compile cost, raw SQLite stepping, and the marginal
//! decode cost of each column representation. Informational (prints a table); run
//! `cargo test -p clove-index --release --test timing_breakdown -- --nocapture`.

use std::time::{Duration, Instant};

use camino::Utf8PathBuf;
use clove_core::fixtures::write_fixtures;
use clove_index::reindex;
use rusqlite::Connection;
use smol_str::SmolStr;
use tempfile::TempDir;

// Matches query.rs: a plain order so the `idx_items_list` covering index serves
// the lean query as an index-only scan.
const ORDER: &str = "ORDER BY priority ASC, topological_rank ASC, id ASC";

fn lean_sql() -> String {
    format!("SELECT id, status, item_type, priority, title FROM items {ORDER}")
}

fn full_sql() -> String {
    format!(
        "SELECT id, title, status, item_type, priority, assignee, parent_id, \
         topological_rank, has_dangling_deps, labels, created_at, updated_at, \
         closed_at, source_system, external_ref FROM items {ORDER}"
    )
}

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

fn best_of(iters: u32, mut op: impl FnMut() -> usize) -> (Duration, usize) {
    let mut best = Duration::MAX;
    let mut last = 0;
    for _ in 0..iters {
        let start = Instant::now();
        last = op();
        best = best.min(start.elapsed());
    }
    (best, last)
}

#[test]
fn ls_timing_breakdown() {
    let n = gate_items();
    let tmp: TempDir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let issues = root.join(".clove").join("issues");
    write_fixtures(&issues, n, 0x6A7E_2026).unwrap();
    let db = root.join(".clove").join("index.db");
    reindex(&issues, &db).unwrap();

    let conn = Connection::open(&db).unwrap();
    let lean = lean_sql();
    let full = full_sql();

    // SQL compile cost (uncached prepare each iteration).
    let (prepare, _) = best_of(50, || {
        let stmt = conn.prepare(&lean).unwrap();
        std::hint::black_box(&stmt);
        0
    });

    // Step every row, read nothing.
    let (step_only, c1) = best_of(20, || {
        let mut stmt = conn.prepare_cached(&lean).unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut count = 0;
        while rows.next().unwrap().is_some() {
            count += 1;
        }
        count
    });

    // Step + read the priority integer column.
    let (read_int, _) = best_of(20, || {
        let mut stmt = conn.prepare_cached(&lean).unwrap();
        let mut rows = stmt.query([]).unwrap();
        let mut sum = 0u64;
        while let Some(r) = rows.next().unwrap() {
            sum += r.get::<_, i64>(3).unwrap() as u64;
        }
        sum as usize
    });

    // Decode the lean row with SmolStr short columns (the real `ls` path).
    let (decode_smol, c2) = best_of(20, || {
        let mut stmt = conn.prepare_cached(&lean).unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    SmolStr::new(r.get_ref(0)?.as_str()?),
                    SmolStr::new(r.get_ref(1)?.as_str()?),
                    SmolStr::new(r.get_ref(2)?.as_str()?),
                    r.get::<_, u8>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .unwrap();
        rows.map(Result::unwrap).count()
    });

    // Decode the lean row with all-String columns.
    let (decode_string, _) = best_of(20, || {
        let mut stmt = conn.prepare_cached(&lean).unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, u8>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .unwrap();
        rows.map(Result::unwrap).count()
    });

    // Decode the full 15-column row incl. the labels-JSON parse (old path).
    let (decode_full, _) = best_of(20, || {
        let mut stmt = conn.prepare_cached(&full).unwrap();
        let rows = stmt
            .query_map([], |r| {
                let labels_json: String = r.get(9)?;
                let labels: Vec<String> = serde_json::from_str(&labels_json).unwrap_or_default();
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, u8>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, bool>(8)?,
                    labels,
                    r.get::<_, String>(10)?,
                    r.get::<_, String>(11)?,
                    r.get::<_, Option<String>>(12)?,
                    r.get::<_, Option<String>>(13)?,
                    r.get::<_, Option<String>>(14)?,
                ))
            })
            .unwrap();
        rows.map(Result::unwrap).count()
    });

    assert_eq!(c1, n);
    assert_eq!(c2, n);

    let us = |d: Duration| d.as_secs_f64() * 1e6;
    let per = |d: Duration| us(d) * 1000.0 / n as f64; // ns/row
    eprintln!("ls timing breakdown @ {n} rows (best of N):");
    eprintln!("  prepare (compile SQL)      {:>8.1} µs", us(prepare));
    eprintln!(
        "  step-only (no decode)      {:>8.1} µs   ({:.0} ns/row)",
        us(step_only),
        per(step_only)
    );
    eprintln!(
        "  + read priority (int)      {:>8.1} µs   ({:.0} ns/row)",
        us(read_int),
        per(read_int)
    );
    eprintln!(
        "  + decode lean (SmolStr)    {:>8.1} µs   ({:.0} ns/row)   <- the `ls` path",
        us(decode_smol),
        per(decode_smol)
    );
    eprintln!(
        "  + decode lean (String)     {:>8.1} µs   ({:.0} ns/row)",
        us(decode_string),
        per(decode_string)
    );
    eprintln!(
        "  + decode full 15-col       {:>8.1} µs   ({:.0} ns/row)   <- old query_items",
        us(decode_full),
        per(decode_full)
    );
    eprintln!("deltas:");
    eprintln!(
        "  stepping is {:.0}% of the lean decode; string-decode marginal = +{:.1} µs over SmolStr",
        100.0 * us(step_only) / us(decode_smol),
        us(decode_string) - us(decode_smol)
    );
}
