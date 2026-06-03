//! Fuzz target (T-X02): the dependency-list parse path. Arbitrary bytes are
//! spliced into the `deps:` block of an otherwise well-formed item, so the YAML
//! sequence parse + dependency validation must never panic on malformed,
//! non-UTF-8, or adversarial dep lists.
//!
//! Run: `cargo +nightly fuzz run parse_dep_list -- -max_total_time=30`.

#![no_main]

use camino::Utf8Path;
use clove_core::fixtures::{deps_fuzz_document, FUZZ_ID};
use clove_core::{parse_item_bytes, CloveId};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let doc = deps_fuzz_document(data);
    let id = CloveId::new(FUZZ_ID).expect("static fuzz id is valid");
    let _ = parse_item_bytes(&doc, Utf8Path::new("fuzz-00000000.md"), &id);
});
