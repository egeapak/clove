//! Memory-footprint comparison for the lean list row (`ItemListRow`): its
//! `SmolStr` short columns vs an all-`String` equivalent, over identical data.
//!
//! A counting global allocator measures the heap bytes and allocation *count*
//! retained by each materialized `Vec` (the source strings are built first and
//! kept alive, so the measurement window captures only the new rows). `SmolStr`
//! stores strings ≤ 23 bytes inline, so `id`/`status`/`type` cost no per-row heap
//! allocation — only `title` heap-allocates.
//!
//! Run with `cargo test -p clove-index --test memory_footprint -- --nocapture`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use clove_index::ItemListRow;
use smol_str::SmolStr;

/// A `System`-backed allocator that tracks outstanding bytes and total count.
struct Counting;

static OUTSTANDING: AtomicI64 = AtomicI64::new(0);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            OUTSTANDING.fetch_add(layout.size() as i64, Ordering::Relaxed);
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        OUTSTANDING.fetch_sub(layout.size() as i64, Ordering::Relaxed);
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// All-`String` mirror of [`ItemListRow`], for the comparison.
#[allow(dead_code)]
struct StringRow {
    id: String,
    status: String,
    item_type: String,
    priority: u8,
    title: String,
}

fn snapshot() -> (i64, usize) {
    (
        OUTSTANDING.load(Ordering::Relaxed),
        ALLOCS.load(Ordering::Relaxed),
    )
}

#[test]
fn smolstr_lean_row_uses_less_memory_than_string() {
    const N: usize = 10_000;

    // Representative source data (kept alive past the measurement window): a
    // 13-byte id, an 11-byte status, a 7-byte type (all inline-able), and a
    // ~35-byte title (always heap).
    let source: Vec<(String, String, String, u8, String)> = (0..N)
        .map(|i| {
            (
                format!("proj-{i:08X}"),
                "in_progress".to_owned(),
                "feature".to_owned(),
                (i % 5) as u8,
                format!("Representative item title number {i}"),
            )
        })
        .collect();

    let before = snapshot();
    let smol: Vec<ItemListRow> = source
        .iter()
        .map(|(id, st, ty, p, ti)| ItemListRow {
            id: SmolStr::new(id.as_str()),
            status: SmolStr::new(st.as_str()),
            item_type: SmolStr::new(ty.as_str()),
            priority: *p,
            title: ti.clone(),
        })
        .collect();
    let after = snapshot();
    let (smol_bytes, smol_allocs) = (after.0 - before.0, after.1 - before.1);
    std::hint::black_box(&smol);

    let before = snapshot();
    let string: Vec<StringRow> = source
        .iter()
        .map(|(id, st, ty, p, ti)| StringRow {
            id: id.clone(),
            status: st.clone(),
            item_type: ty.clone(),
            priority: *p,
            title: ti.clone(),
        })
        .collect();
    let after = snapshot();
    let (string_bytes, string_allocs) = (after.0 - before.0, after.1 - before.1);
    std::hint::black_box(&string);

    let pct = |saved: i64, base: i64| 100.0 * saved as f64 / base as f64;
    eprintln!("lean-row memory @ {N} rows:");
    eprintln!(
        "  SmolStr: {smol_bytes:>9} B  {smol_allocs:>7} allocs   ({:.1} B/row, {:.2} allocs/row)",
        smol_bytes as f64 / N as f64,
        smol_allocs as f64 / N as f64
    );
    eprintln!(
        "  String:  {string_bytes:>9} B  {string_allocs:>7} allocs   ({:.1} B/row, {:.2} allocs/row)",
        string_bytes as f64 / N as f64,
        string_allocs as f64 / N as f64
    );
    eprintln!(
        "  saved:   {:>9} B ({:.0}%)  {:>7} allocs ({:.0}%)",
        string_bytes - smol_bytes,
        pct(string_bytes - smol_bytes, string_bytes),
        string_allocs as i64 - smol_allocs as i64,
        pct((string_allocs - smol_allocs) as i64, string_allocs as i64),
    );

    assert!(
        smol_bytes < string_bytes,
        "SmolStr lean row must retain fewer bytes ({smol_bytes} vs {string_bytes})"
    );
    assert!(
        smol_allocs < string_allocs,
        "SmolStr lean row must do fewer allocations ({smol_allocs} vs {string_allocs})"
    );
}
