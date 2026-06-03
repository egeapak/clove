//! Fuzz target (T-X02): parsing arbitrary bytes as a single tk ticket file must
//! never panic — only return `Ok` or an `ImportError`.
//!
//! Exercises the tk importer's parse path (frontmatter split + tolerant
//! `TkTicket` deserialization + `# H1` extraction) via
//! [`clove_import::tk::parse_ticket_bytes`]. No filesystem access, no writes.
//!
//! Run: `cargo +nightly fuzz run import_tk -- -max_total_time=30`
//! (CI runs each target for 30s against the committed `corpus/`).

#![no_main]

use clove_import::tk::parse_ticket_bytes;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_ticket_bytes(data);
});
