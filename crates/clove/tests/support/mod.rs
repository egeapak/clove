//! Shared by the suites that spawn a real daemon or a plugin binary.
//!
//! Cargo gives an integration test only its own package's binaries, so
//! `cloved` and the plugins come from elsewhere: the nextest setup script
//! (`.config/nextest.toml`, `cargo xtask test-bins`) builds them once per run,
//! before any test, and passes their directory in `CLOVE_TEST_BIN_DIR`. These
//! helpers only look the binaries up, so every test process of a run spawns the
//! same files and no test ever builds or replaces one. Plain `cargo test` does
//! not run the setup script, so these suites fail with a message naming nextest.
// Each test binary uses a subset of these helpers.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The environment that keeps a spawned `clove` away from the developer's own
/// hub and clove home: a private runtime directory (where a hub's socket, lock
/// and pid file live) and a private clove home (token records, managed
/// plugins), both under `dir`.
///
/// Without `CLOVE_RUNTIME_DIR` the client probes the user's real runtime
/// directory, attaches to a hub found there, and may clean up after one it
/// judges dead. Every suite passes this to its `clove` command first, then sets
/// what it needs on top (`cmd.envs(support::isolated_env(dir))`).
///
/// `HOME` stays as it is: clove reads it only to find the clove home, which
/// `CLOVE_HOME` overrides, and the `git` and `cargo` that some suites spawn
/// need the real one.
pub fn isolated_env(dir: &Path) -> [(&'static str, PathBuf); 2] {
    [
        ("CLOVE_RUNTIME_DIR", dir.join(".test-run")),
        ("CLOVE_HOME", dir.join(".test-clove-home")),
    ]
}

/// The `cloved` built for this test run (all features, so the `CLOVED_*` test
/// knobs and the web server are both present).
#[cfg(unix)]
pub fn cloved() -> &'static Path {
    static CLOVED: OnceLock<PathBuf> = OnceLock::new();
    CLOVED.get_or_init(|| existing(bin_dir().join(exe("cloved"))))
}

/// A plugin binary built for this test run, beside the `clove` under test.
///
/// It must live there: the host searches the running `clove`'s own directory
/// before `CLOVE_PLUGIN_PATH`, so a plugin resolved by name runs whatever sits
/// beside `cargo_bin("clove")`. The setup script puts this run's build there;
/// this checks that the file there is that build and not one left over. The
/// check runs once per plugin in a test process.
pub fn plugin(bin: &str) -> &'static Path {
    static CHECKED: OnceLock<Mutex<HashMap<String, &'static Path>>> = OnceLock::new();
    let mut checked = CHECKED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(path) = checked.get(bin) {
        return path;
    }
    let beside_clove = existing(clove_bin_dir().join(exe(bin)));
    let built = existing(bin_dir().join(exe(bin)));
    assert!(
        same_content(&beside_clove, &built),
        "{} is not the {} that this run built; run the tests with `cargo nextest run`",
        beside_clove.display(),
        built.display(),
    );
    let path: &'static Path = Box::leak(beside_clove.into_boxed_path());
    checked.insert(bin.to_owned(), path);
    path
}

/// Whether two files hold the same bytes. The setup script hard-links the
/// plugin beside `clove`, so on Unix the usual answer is "same inode" with
/// nothing read. Equal length alone is not proof: a stale build of the same
/// size would pass.
fn same_content(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            if (a.dev(), a.ino()) == (b.dev(), b.ino()) {
                return true;
            }
        }
    }
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The directory the setup script built the binaries into.
fn bin_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| match std::env::var_os("CLOVE_TEST_BIN_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => panic!(
            "CLOVE_TEST_BIN_DIR is not set. Run the tests with `cargo nextest run`: \
             its setup script (`cargo xtask test-bins`) builds the cloved and plugin \
             binaries that these tests spawn."
        ),
    })
}

/// The directory of the `clove` binary the tests run (`cargo_bin("clove")`).
/// nextest sets `CARGO_BIN_EXE_*` at run time; the compile-time value is the
/// fallback.
fn clove_bin_dir() -> PathBuf {
    let clove = std::env::var_os("CARGO_BIN_EXE_clove")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_clove")));
    clove
        .parent()
        .expect("the clove binary has a parent dir")
        .to_path_buf()
}

fn exe(bin: &str) -> String {
    format!("{bin}{}", std::env::consts::EXE_SUFFIX)
}

fn existing(path: PathBuf) -> PathBuf {
    assert!(
        path.is_file(),
        "{} does not exist; run the tests with `cargo nextest run`, whose setup \
         script (`cargo xtask test-bins`) builds it",
        path.display()
    );
    path
}
