//! Fuzz corpus replay (T-X02 regression on stable): runs every committed seed
//! in `fuzz/corpus/<target>/` through the exact parse path its fuzz target
//! drives, asserting none panics. This keeps the "no panic on the committed
//! corpus" acceptance gate enforceable in ordinary `cargo test`, without needing
//! nightly + cargo-fuzz installed. The deep fuzzing itself still runs via
//! `cargo +nightly fuzz run` in CI.

use std::path::{Path, PathBuf};

use camino::Utf8Path;
use clove_core::fixtures::{deps_fuzz_document, FUZZ_ID};
use clove_core::parse_item_bytes;
use clove_types::CloveId;

/// Path to `fuzz/corpus/<target>/`, resolved from this crate's manifest dir.
fn corpus_dir(target: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fuzz")
        .join("corpus")
        .join(target)
}

/// Read every seed file in `dir` (non-recursive), returning (name, bytes).
fn seeds(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("corpus dir exists") {
        let path = entry.unwrap().path();
        if path.is_file() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            out.push((name, std::fs::read(&path).unwrap()));
        }
    }
    out
}

#[test]
fn parse_item_file_corpus_never_panics() {
    let id = CloveId::new(FUZZ_ID).unwrap();
    let path = Utf8Path::new("fuzz-00000000.md");
    let seeds = seeds(&corpus_dir("parse_item_file"));
    assert!(
        !seeds.is_empty(),
        "parse_item_file corpus must not be empty"
    );

    let mut ok_seen = false;
    for (name, bytes) in &seeds {
        // The contract under test: never panics — Ok or Err only.
        if parse_item_bytes(bytes, path, &id).is_ok() {
            ok_seen = true;
        }
        eprintln!("replayed parse_item_file seed: {name}");
    }
    assert!(
        ok_seen,
        "at least one valid seed should parse (harness wired)"
    );
}

#[test]
fn parse_dep_list_corpus_never_panics() {
    let id = CloveId::new(FUZZ_ID).unwrap();
    let path = Utf8Path::new("fuzz-00000000.md");
    let seeds = seeds(&corpus_dir("parse_dep_list"));
    assert!(!seeds.is_empty(), "parse_dep_list corpus must not be empty");

    for (name, bytes) in &seeds {
        let doc = deps_fuzz_document(bytes);
        // Must never panic on a spliced (possibly malformed) dependency list.
        let _ = parse_item_bytes(&doc, path, &id);
        eprintln!("replayed parse_dep_list seed: {name}");
    }
}
