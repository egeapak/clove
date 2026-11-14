//! cargo xtask — clove build/bench tooling.
//!
//! - `bench-fixtures --count N --out-dir PATH [--seed S]` writes N deterministic
//!   item files (DESIGN §13.4 profile) via the shared `clove_core::fixtures`
//!   generator — the same one the criterion benches and perf-gate tests use.
//! - `test-all` runs the workspace test suite with cargo-nextest, then the
//!   doctests (nextest does not run them). The suite replays the committed
//!   fuzz seed corpora of all five fuzz targets (`tests/fuzz_corpus.rs` in
//!   `clove-core` and in `clove-import`).
//! - `test-bins` builds the `cloved` and plugin binaries that the daemon and
//!   plugin suites of `clove-cli` spawn. It is the nextest setup script in
//!   `.config/nextest.toml`, so it runs once per test run, before any test.
//! - `publish-order` prints the crates.io publish order, one crate per line,
//!   derived from the workspace dependency graph (`scripts/release/publish.sh`
//!   reads it).
//! - `guard-sdk-licenses`, `guard-no-web` and `guard-tls` are the dependency
//!   tree guards that CI runs (see `guards.rs`).
//! - `guard-glibc <floor> <binary>...` fails when a Linux binary requires a
//!   newer glibc than `<floor>`. The release workflow runs it.

mod guards;
mod publish;

use camino::Utf8PathBuf;
use clove_core::fixtures::write_fixtures;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("bench-fixtures") => bench_fixtures(args.collect()),
        Some("test-all") => test_all(),
        Some("test-bins") => test_bins(),
        Some("publish-order") => publish::print_order(),
        Some("guard-sdk-licenses") => guards::sdk_licenses(),
        Some("guard-no-web") => guards::no_web(),
        Some("guard-tls") => guards::tls(),
        Some("guard-glibc") => guards::glibc(args.collect()),
        Some(other) => {
            eprintln!("xtask: unknown command `{other}`");
            std::process::exit(1);
        }
        None => {
            eprintln!(
                "usage: cargo xtask <bench-fixtures|test-all|test-bins|publish-order|\
                 guard-sdk-licenses|guard-no-web|guard-tls|guard-glibc>"
            );
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
    for args in [
        &["nextest", "run", "--workspace", "--all-features"][..],
        &["test", "--workspace", "--all-features", "--doc"][..],
    ] {
        let status = cargo().args(args).status()?;
        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }
    }
    Ok(())
}

/// The packages whose binaries the `clove-cli` suites spawn.
const TEST_BIN_PACKAGES: [&str; 5] = [
    "cloved",
    "clove-sync-github",
    "clove-import-tk",
    "clove-import-beads",
    "clove-plugin-echo",
];

/// The plugin binaries, which must sit beside the `clove` under test: the host
/// searches the running `clove`'s own directory first.
const TEST_PLUGINS: [&str; 5] = [
    "clove-sync-github",
    "clove-import-tk",
    "clove-import-beads",
    "clove-echo",
    "fake-cargo",
];

/// Build every binary the `clove-cli` daemon and plugin suites spawn, once, so
/// every test process of a run uses the same files.
///
/// The build uses its own artifact dir (`target/test-bins`) but the workspace
/// build dir, so it reuses what nextest already compiled. `--profile test
/// --workspace --all-features --tests` gives the same units as
/// `cargo nextest run --workspace --all-features`, and `--bins` adds the real
/// plugin executables (`cargo test` builds those crates only as unit-test
/// harnesses). After that run's own build, it only links those few binaries. A
/// separate artifact dir means it never relinks the `clove` or `cloved` that the
/// run itself tests.
///
/// Then the plugins are copied beside `target/debug/clove`, and the
/// `CLOVE_TEST_BIN_DIR` that `crates/clove/tests/support` reads is written to
/// `$NEXTEST_ENV`. Every test starts after this returns, so nothing can be
/// spawning the files that are replaced.
fn test_bins() -> anyhow::Result<()> {
    let root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace")
        .to_owned();
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|_| root.join("target"));
    let artifact_dir = target.join("test-bins");

    let mut build = cargo();
    build
        .current_dir(&root)
        .args([
            "build",
            "--profile",
            "test",
            "--workspace",
            "--all-features",
            "--bins",
            "--tests",
        ])
        .arg("--target-dir")
        .arg(&artifact_dir)
        .env("CARGO_BUILD_BUILD_DIR", &target);
    // This build must see the environment that nextest's own build saw, or
    // build scripts that track a variable (ring tracks CARGO_MANIFEST_DIR) rerun
    // here, and nextest's build reruns them back on the next run. `cargo run`
    // (the `cargo xtask` alias) sets the xtask package's variables, and both
    // `cargo run` and nextest add target-dir paths to the library search path.
    for (var, _) in std::env::vars_os() {
        let name = var.to_string_lossy();
        if name.starts_with("CARGO_PKG_")
            || [
                "CARGO_MANIFEST_DIR",
                "CARGO_MANIFEST_PATH",
                "CARGO_CRATE_NAME",
                "CARGO_BIN_NAME",
                "CARGO_PRIMARY_PACKAGE",
            ]
            .contains(&name.as_ref())
        {
            build.env_remove(&var);
        }
    }
    for var in [
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "PATH",
    ] {
        if let Some(value) = std::env::var_os(var) {
            let kept: Vec<_> = std::env::split_paths(&value)
                .filter(|dir| !dir.starts_with(&target))
                .collect();
            if kept.is_empty() {
                build.env_remove(var);
            } else {
                build.env(var, std::env::join_paths(kept)?);
            }
        }
    }
    for package in TEST_BIN_PACKAGES {
        // `--workspace` already selects them; naming them fails the build
        // loudly if one is ever renamed or removed.
        build.args(["--package", package]);
    }
    let status = build.status()?;
    anyhow::ensure!(status.success(), "building the test binaries failed");

    let bin_dir = artifact_dir.join("debug");
    let clove_dir = target.join("debug");
    std::fs::create_dir_all(&clove_dir)?;
    for plugin in TEST_PLUGINS {
        let name = format!("{plugin}{}", std::env::consts::EXE_SUFFIX);
        let (from, to) = (bin_dir.join(&name), clove_dir.join(&name));
        // Remove first: a hard link or copy onto an existing file can fail,
        // and the old file may be a hard link into the build dir.
        let _ = std::fs::remove_file(&to);
        if std::fs::hard_link(&from, &to).is_err() {
            std::fs::copy(&from, &to)
                .map_err(|err| anyhow::anyhow!("copy {from} to {to}: {err}"))?;
        }
    }

    match std::env::var_os("NEXTEST_ENV") {
        Some(env_file) => {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().append(true).open(env_file)?;
            writeln!(file, "CLOVE_TEST_BIN_DIR={bin_dir}")?;
        }
        None => println!("CLOVE_TEST_BIN_DIR={bin_dir}"),
    }
    Ok(())
}

/// The cargo that runs this xtask, so a toolchain override carries over.
fn cargo() -> std::process::Command {
    std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}
