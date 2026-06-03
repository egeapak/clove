//! Fuzz target (T-X02): parsing arbitrary bytes as a clove item file must never
//! panic — only return `Ok(Item)` or a `CloveError`.
//!
//! Run: `cargo +nightly fuzz run parse_item_file -- -max_total_time=30`
//! (CI runs each target for 30s against the committed `corpus/`).

#![no_main]

use camino::Utf8Path;
use clove_core::fixtures::FUZZ_ID;
use clove_core::{parse_item_bytes, CloveId};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let id = CloveId::new(FUZZ_ID).expect("static fuzz id is valid");
    // The frontmatter + body parse is the surface under test; the trailing id
    // check is incidental. No input may cause a panic.
    let _ = parse_item_bytes(data, Utf8Path::new("fuzz-00000000.md"), &id);
});
