//! Fuzz target (T-X02): parsing arbitrary bytes as a beads `issues.jsonl`
//! document must never panic — only return `Ok` or an `ImportError`.
//!
//! Exercises the beads importer's parse path (line split + tolerant `BeadsIssue`
//! deserialization + field mapping) via
//! [`clove_import::beads::parse_beads_bytes`]. No filesystem access, no writes.
//!
//! Run: `cargo +nightly fuzz run import_beads -- -max_total_time=30`
//! (CI runs each target for 30s against the committed `corpus/`).

#![no_main]

use clove_import::beads::parse_beads_bytes;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_beads_bytes(data);
});
