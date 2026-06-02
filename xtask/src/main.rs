//! cargo xtask — clove build/bench tooling.
//!
//! `bench-fixtures --count N --out-dir PATH` writes N deterministic item files
//! following the DESIGN §13.4 statistical profile (used by benchmarks and tests).
//! `test-all` runs the workspace test suite.

use std::io::Write;

use camino::Utf8PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("bench-fixtures") => bench_fixtures(args.collect()),
        Some("test-all") => test_all(),
        Some(other) => {
            eprintln!("xtask: unknown command `{other}`");
            std::process::exit(1);
        }
        None => {
            eprintln!("usage: cargo xtask <bench-fixtures|test-all>");
            std::process::exit(1);
        }
    }
}

/// Deterministic xorshift64 PRNG — no external dependency, reproducible fixtures.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// Encode `n` as an 8-char Crockford-base32 suffix (valid `CloveId` tail).
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
    String::from_utf8(buf.to_vec()).unwrap()
}

fn bench_fixtures(args: Vec<String>) -> anyhow::Result<()> {
    let mut count = 1000usize;
    let mut out_dir = Utf8PathBuf::from("bench-fixtures");
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--count" => count = it.next().unwrap_or_default().parse()?,
            "--out-dir" => out_dir = Utf8PathBuf::from(it.next().unwrap_or_default()),
            other => anyhow::bail!("unknown flag `{other}`"),
        }
    }
    std::fs::create_dir_all(&out_dir)?;

    let mut rng = Rng::new(0x5eed_1234_abcd_ef01);
    let ids: Vec<String> = (0..count)
        .map(|i| format!("bench-{}", suffix(i as u64)))
        .collect();

    for (i, id) in ids.iter().enumerate() {
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

        // 20% of items get 1–4 deps on strictly-earlier items (acyclic).
        let mut deps_block = String::new();
        if i > 0 && rng.below(100) < 20 {
            let want = 1 + rng.below(4) as usize;
            let mut chosen = std::collections::BTreeSet::new();
            for _ in 0..want {
                chosen.insert(rng.below(i as u64) as usize);
            }
            deps_block.push_str("deps:\n");
            for d in chosen {
                deps_block.push_str(&format!("  - {}\n", ids[d]));
            }
        }

        let body = format!(
            "---\nschema: 1\nid: {id}\ntitle: Benchmark item {i}\nstatus: {status}\n\
             type: {item_type}\npriority: {priority}\n\
             created: 2026-01-01T00:00:00Z\nupdated: 2026-01-01T00:00:00Z\n{closed_line}{deps_block}\
             ---\nGenerated benchmark item {i} with searchable keyword{i}.\n"
        );
        let mut file = std::fs::File::create(out_dir.join(format!("{id}.md")))?;
        file.write_all(body.as_bytes())?;
    }

    println!("wrote {count} fixture items to {out_dir}");
    Ok(())
}

fn test_all() -> anyhow::Result<()> {
    let status = std::process::Command::new(env!("CARGO"))
        .args(["test", "--workspace"])
        .status()?;
    std::process::exit(status.code().unwrap_or(1));
}
