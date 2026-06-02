//! clove CLI entry point.
//!
//! Thin shell over `clove-core` (and, from M1, `clove-index`). JSON everywhere;
//! exit codes per DESIGN.md §7.6. Real command wiring lands in the T-CLI* tasks.

fn main() {
    println!("clove {}", env!("CARGO_PKG_VERSION"));
}
