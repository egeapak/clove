//! Fuzz corpus replay on stable for the three fuzz targets that drive this
//! crate (`fuzz/fuzz_targets/{merge_driver,import_tk,import_beads}.rs`): every
//! committed seed in `fuzz/corpus/<target>/` goes through the same code path as
//! its target, and none may panic. `clove-core/tests/fuzz_corpus.rs` does the
//! same for the two parser targets. The deep fuzzing still runs with
//! `cargo +nightly fuzz run` in CI; this keeps the committed seeds enforced in
//! an ordinary test run, without nightly or cargo-fuzz.

use std::path::{Path, PathBuf};

use camino::Utf8Path;
use clove_core::parse_item_lenient;
use clove_import::beads::parse_beads_bytes;
use clove_import::merge::merge_frontmatter;
use clove_import::tk::parse_ticket_bytes;

/// Path to `fuzz/corpus/<target>/`, resolved from this crate's manifest dir.
fn corpus_dir(target: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fuzz")
        .join("corpus")
        .join(target)
}

/// Every seed file of a target (non-recursive), as (name, bytes). A missing or
/// empty corpus fails the test: a replay of nothing proves nothing.
fn seeds(target: &str) -> Vec<(String, Vec<u8>)> {
    let dir = corpus_dir(target);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_file() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            out.push((name, std::fs::read(&path).unwrap()));
        }
    }
    assert!(!out.is_empty(), "the {target} corpus must not be empty");
    out
}

/// Mirrors `fuzz_targets/merge_driver.rs`: split on NUL into base / ours /
/// theirs, parse each leniently, and merge when both sides parse.
#[test]
fn merge_driver_corpus_never_panics() {
    let path = Utf8Path::new("fuzz-00000000.md");
    let mut merged = 0;
    for (name, bytes) in seeds("merge_driver") {
        let mut parts = bytes.splitn(3, |&b| b == 0);
        let base = parse_item_lenient(parts.next().unwrap_or(b""), path).ok();
        let ours = parse_item_lenient(parts.next().unwrap_or(b""), path).ok();
        let theirs = parse_item_lenient(parts.next().unwrap_or(b""), path).ok();
        if let (Some(ours), Some(theirs)) = (ours, theirs) {
            let base = base.as_ref().map(|item| &item.frontmatter);
            let _ = merge_frontmatter(base, &ours.frontmatter, &theirs.frontmatter);
            merged += 1;
        }
        eprintln!("replayed merge_driver seed: {name}");
    }
    assert!(
        merged > 0,
        "at least one seed should reach the merge (harness wired)"
    );
}

/// Mirrors `fuzz_targets/import_tk.rs`.
#[test]
fn import_tk_corpus_never_panics() {
    let mut ok_seen = false;
    for (name, bytes) in seeds("import_tk") {
        // The contract under test: never panics — Ok or Err only.
        ok_seen |= parse_ticket_bytes(&bytes).is_ok();
        eprintln!("replayed import_tk seed: {name}");
    }
    assert!(
        ok_seen,
        "at least one valid seed should parse (harness wired)"
    );
}

/// Mirrors `fuzz_targets/import_beads.rs`.
#[test]
fn import_beads_corpus_never_panics() {
    let mut ok_seen = false;
    for (name, bytes) in seeds("import_beads") {
        ok_seen |= parse_beads_bytes(&bytes).is_ok();
        eprintln!("replayed import_beads seed: {name}");
    }
    assert!(
        ok_seen,
        "at least one valid seed should parse (harness wired)"
    );
}
