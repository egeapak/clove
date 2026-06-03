//! Fuzz target (T-X02): the pure three-way frontmatter merge must never panic.
//!
//! Arbitrary input bytes are split into three blobs (`base` / `ours` / `theirs`)
//! on a NUL separator, each parsed via the tolerant clove-item parse path. Any
//! blob that parses contributes its frontmatter to a [`merge_frontmatter`] call.
//! The merge math (scalars, set merges, status/closed coupling) must terminate
//! and never panic on any combination of valid/garbage inputs.
//!
//! Run: `cargo +nightly fuzz run merge_driver -- -max_total_time=30`
//! (CI runs each target for 30s against the committed `corpus/`).

#![no_main]

use camino::Utf8Path;
use clove_core::parse_item_lenient;
use clove_import::merge::merge_frontmatter;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Split into up to three blobs on NUL; missing parts default to empty.
    let mut parts = data.splitn(3, |&b| b == 0);
    let base_bytes = parts.next().unwrap_or(b"");
    let ours_bytes = parts.next().unwrap_or(b"");
    let theirs_bytes = parts.next().unwrap_or(b"");

    let path = Utf8Path::new("fuzz-00000000.md");
    let base = parse_item_lenient(base_bytes, path).ok();
    let ours = parse_item_lenient(ours_bytes, path).ok();
    let theirs = parse_item_lenient(theirs_bytes, path).ok();

    // Need at least both sides to merge; otherwise nothing to exercise.
    if let (Some(ours), Some(theirs)) = (ours, theirs) {
        let base_fm = base.as_ref().map(|i| &i.frontmatter);
        let _ = merge_frontmatter(base_fm, &ours.frontmatter, &theirs.frontmatter);
    }
});
