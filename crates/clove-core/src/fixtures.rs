//! Deterministic, seeded benchmark/test fixture generation (DESIGN.md §13.4).
//!
//! [`write_fixtures`] writes `count` valid item files into `out_dir` following
//! the documented statistical profile (25% closed / 10% in_progress / 65% open;
//! 20% with 1–4 deps on earlier items; 30% with a label; mixed body sizes).
//! Output is fully determined by `seed`, so benchmarks and the performance-gate
//! tests are reproducible. Shared by `cargo xtask bench-fixtures`, the criterion
//! benches, and `tests/perf_gates.rs`.

use std::io::{self, Write};

use camino::Utf8Path;

use crate::id::CloveId;

/// A deterministic xorshift64 PRNG — no external dependency, reproducible runs.
pub struct Rng(u64);

impl Rng {
    /// Seed the generator. A zero seed is bumped to 1 (xorshift fixed point).
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    /// Next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform value in `0..n` (`n` must be non-zero).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// Encode `n` as an 8-char Crockford-base32 suffix — a valid [`CloveId`] tail.
fn suffix(n: u64) -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut buf = [b'0'; 8];
    let mut value = n;
    let mut i = 8;
    while value > 0 && i > 0 {
        i -= 1;
        buf[i] = ALPHABET[(value % 32) as usize];
        value /= 32;
    }
    String::from_utf8(buf.to_vec()).expect("base32 alphabet is ASCII")
}

/// Twenty realistic label values, drawn from for the 30% of items that get one.
const LABELS: &[&str] = &[
    "area:core",
    "area:cli",
    "area:index",
    "area:graph",
    "area:docs",
    "kind:regression",
    "kind:perf",
    "kind:cleanup",
    "prio:now",
    "prio:later",
    "team:backend",
    "team:tooling",
    "risk:high",
    "risk:low",
    "needs:design",
    "needs:review",
    "blocked:external",
    "good-first-issue",
    "flaky",
    "tech-debt",
];

/// Write `count` deterministic item files into `out_dir`, returning their ids in
/// creation order. `out_dir` is created if it does not exist.
pub fn write_fixtures(out_dir: &Utf8Path, count: usize, seed: u64) -> io::Result<Vec<CloveId>> {
    std::fs::create_dir_all(out_dir)?;

    let mut rng = Rng::new(seed);
    let id_strings: Vec<String> = (0..count)
        .map(|i| format!("bench-{}", suffix(i as u64)))
        .collect();

    for (i, id) in id_strings.iter().enumerate() {
        // Status: 25% closed, 10% in_progress, 65% open.
        let roll = rng.below(100);
        let (status, closed_line) = if roll < 25 {
            ("closed", "closed: 2026-01-02T00:00:00Z\n".to_owned())
        } else if roll < 35 {
            ("in_progress", String::new())
        } else {
            ("open", String::new())
        };
        let priority = rng.below(5);
        let item_type = ["bug", "feature", "chore", "docs"][rng.below(4) as usize];

        // 30% of items get one label from the realistic pool.
        let labels_block = if rng.below(100) < 30 {
            let label = LABELS[rng.below(LABELS.len() as u64) as usize];
            format!("labels:\n  - {label}\n")
        } else {
            String::new()
        };

        // 20% of items get 1–4 deps on strictly-earlier items (so the corpus is
        // acyclic and the ready/blocked queries have real work to do).
        let mut deps_block = String::new();
        if i > 0 && rng.below(100) < 20 {
            let want = 1 + rng.below(4) as usize;
            let mut chosen = std::collections::BTreeSet::new();
            for _ in 0..want {
                chosen.insert(rng.below(i as u64) as usize);
            }
            deps_block.push_str("deps:\n");
            for d in chosen {
                deps_block.push_str(&format!("  - {}\n", id_strings[d]));
            }
        }

        // Body size: 85% short, 10% medium, 5% long.
        let body_roll = rng.below(100);
        let filler = if body_roll < 85 {
            "Short benchmark body with a searchable token.".to_owned()
        } else if body_roll < 95 {
            "Medium benchmark body. ".repeat(9)
        } else {
            "Long benchmark body paragraph. ".repeat(23)
        };

        let document = format!(
            "---\nschema: 1\nid: {id}\ntitle: Benchmark item {i} with a representative title\n\
             status: {status}\ntype: {item_type}\npriority: {priority}\n\
             created: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\n\
             {closed_line}{labels_block}{deps_block}---\n{filler} keyword{i}.\n"
        );
        let mut file = std::fs::File::create(out_dir.join(format!("{id}.md")))?;
        file.write_all(document.as_bytes())?;
    }

    Ok(id_strings
        .iter()
        .map(|s| CloveId::new(s).expect("generated ids are valid"))
        .collect())
}

/// Splice arbitrary `deps_bytes` into the `deps:` block of an otherwise
/// well-formed item document, returning the raw bytes to parse.
///
/// Shared by the `parse_dep_list` fuzz target and its corpus-replay regression
/// test so both exercise byte-for-byte the same dependency-list parse path.
pub fn deps_fuzz_document(deps_bytes: &[u8]) -> Vec<u8> {
    let mut doc = Vec::with_capacity(deps_bytes.len() + 256);
    doc.extend_from_slice(
        b"---\nschema: 1\nid: fuzz-00000000\ntitle: fuzz\nstatus: open\n\
          type: bug\npriority: 0\ncreated: 2026-01-01T00:00:00Z\n\
          updated: 2026-01-01T00:00:00Z\ndeps:\n",
    );
    doc.extend_from_slice(deps_bytes);
    doc.extend_from_slice(b"\n---\nbody\n");
    doc
}

/// The stable id used by the fuzz targets and their corpus-replay test.
pub const FUZZ_ID: &str = "fuzz-00000000";
