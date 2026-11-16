//! Deterministic, seeded benchmark/test fixture generation (DESIGN.md §13.4).
//!
//! [`write_fixtures`] writes `count` valid item files into `out_dir` following
//! the documented statistical profile (25% closed / 10% in_progress / 65% open;
//! 20% with 1–4 deps on earlier items; 30% with a label; mixed body sizes).
//! Output is fully determined by `seed`, so benchmarks and the performance-gate
//! tests are reproducible. Shared by `cargo xtask bench-fixtures`, the criterion
//! benches, and `tests/perf_gates.rs`.
//!
//! [`perf_gate`] is the measurement that the performance-gate tests share.

use std::io::{self, Write};
use std::time::{Duration, Instant};

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

/// How many rounds a performance gate measures before it fails.
pub const GATE_ROUNDS: u32 = 3;

/// The pause between two rounds of a performance gate, so that a short burst
/// of load on the machine can pass.
pub const GATE_PAUSE: Duration = Duration::from_millis(250);

/// The result of one performance gate: the fastest time of each round that ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRun {
    /// The budget that the gate measured against.
    pub budget: Duration,
    /// The fastest time of each round, in the order that the rounds ran.
    pub rounds: Vec<Duration>,
}

impl GateRun {
    /// Whether a round finished within the budget.
    pub fn passed(&self) -> bool {
        self.rounds.iter().any(|round| *round <= self.budget)
    }

    /// The fastest time of all rounds.
    pub fn best(&self) -> Duration {
        self.rounds.iter().copied().min().unwrap_or(Duration::MAX)
    }
}

/// Measure `op` against `budget` for a performance-gate test.
///
/// A round runs `op` `iters` times and keeps the fastest time. When a round is
/// within the budget, the gate stops and passes. Otherwise it waits `pause` and
/// runs another round, up to `rounds` rounds. Load on a shared machine slows
/// some rounds, but a real regression slows every round, so the gate fails only
/// when every round is over the budget.
pub fn perf_gate(
    rounds: u32,
    iters: u32,
    budget: Duration,
    pause: Duration,
    mut op: impl FnMut(),
) -> GateRun {
    let mut run = GateRun {
        budget,
        rounds: Vec::new(),
    };
    for round in 0..rounds.max(1) {
        if round > 0 {
            std::thread::sleep(pause);
        }
        let mut best = Duration::MAX;
        for _ in 0..iters.max(1) {
            let start = Instant::now();
            op();
            best = best.min(start.elapsed());
        }
        run.rounds.push(best);
        if best <= budget {
            break;
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_millis(50);
    const SLOW: Duration = Duration::from_millis(80);

    #[test]
    fn a_gate_within_budget_stops_after_one_round() {
        let run = perf_gate(3, 2, BUDGET, Duration::ZERO, || {});
        assert!(run.passed());
        assert_eq!(run.rounds.len(), 1);
    }

    #[test]
    fn a_slow_round_under_load_does_not_fail_the_gate() {
        // The first round is slow (a burst of load), the second is fast.
        let mut calls = 0;
        let run = perf_gate(3, 1, BUDGET, Duration::ZERO, || {
            calls += 1;
            if calls == 1 {
                std::thread::sleep(SLOW);
            }
        });
        assert!(run.passed(), "{run:?}");
        assert_eq!(run.rounds.len(), 2);
        assert!(run.rounds[0] > BUDGET);
    }

    #[test]
    fn a_regression_fails_every_round() {
        let run = perf_gate(3, 2, BUDGET, Duration::ZERO, || std::thread::sleep(SLOW));
        assert!(!run.passed(), "{run:?}");
        assert_eq!(run.rounds.len(), 3);
        assert!(run.best() > BUDGET);
    }
}
