//! cargo xtask — clove build/bench tooling.
//!
//! - `bench-fixtures --count N --out-dir PATH [--seed S]` writes N deterministic
//!   item files (DESIGN §13.4 profile) via the shared `clove_core::fixtures`
//!   generator — the same one the criterion benches and perf-gate tests use.
//! - `test-all` runs the workspace test suite (which includes the fuzz seed
//!   corpus replay in `clove-core`).

use camino::Utf8PathBuf;
use clove_core::fixtures::write_fixtures;

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

fn bench_fixtures(args: Vec<String>) -> anyhow::Result<()> {
    let mut count = 1000usize;
    let mut out_dir = Utf8PathBuf::from("bench-fixtures");
    let mut seed = 0x5eed_1234_abcd_ef01u64;
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--count" => count = it.next().unwrap_or_default().parse()?,
            "--out-dir" => out_dir = Utf8PathBuf::from(it.next().unwrap_or_default()),
            "--seed" => seed = it.next().unwrap_or_default().parse()?,
            other => anyhow::bail!("unknown flag `{other}`"),
        }
    }

    let ids = write_fixtures(&out_dir, count, seed)?;
    println!("wrote {} fixture items to {out_dir}", ids.len());
    Ok(())
}

fn test_all() -> anyhow::Result<()> {
    let status = std::process::Command::new(env!("CARGO"))
        .args(["test", "--workspace"])
        .status()?;
    std::process::exit(status.code().unwrap_or(1));
}
