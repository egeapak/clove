//! Shared by the suites that spawn a real daemon or a plugin binary.
//!
//! Every escargot build goes through [`cloved`] or [`plugin`]. Two facts shape
//! them:
//!
//! - Every `cargo build`, even a no-op one, deletes and re-hard-links the binary
//!   it reports. A test that spawned that path while another build ran would
//!   see the binary briefly vanish (`ENOENT`).
//! - cargo-nextest runs every test in its own process, so a process-wide
//!   `OnceLock` does not stop two tests from building the same artifact at once.
//!
//! So no test spawns the path cargo reports. Each artifact is built into its own
//! scratch target dir, under a cross-process file lock, and then *published*:
//! hard-linked (or copied) to a temporary name and renamed onto the path the
//! tests use. A rename is atomic, so a spawn sees the old binary or the new one,
//! never a missing one. The rename happens only when the bytes changed, so a
//! no-op rebuild leaves the published file alone (and Windows never has to
//! replace a running executable). Under nextest, a stamp holding
//! `NEXTEST_RUN_ID` lets every later process of the same run reuse the
//! published binary without starting cargo at all.
// Each test binary uses a subset of these helpers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// `cloved` built with `features` (space-separated; `""` for the defaults).
///
/// Each feature set gets its own artifact dir, so two suites asking for
/// different features never replace each other's binary, and none of them
/// overwrites the `cloved` in the workspace `target/<profile>/` that
/// `crates/cloved/tests` run (`CARGO_BIN_EXE_cloved`). The artifact dirs share
/// the workspace build dir, so a feature set is compiled once, not once per dir.
#[cfg(unix)]
pub fn cloved(features: &str) -> &'static Path {
    built("cloved", "cloved", features, Placement::Isolated)
}

/// A plugin binary built into the workspace `target/<profile>/`.
///
/// It must live there: the host searches the running `clove`'s own directory
/// before `CLOVE_PLUGIN_PATH`, so a plugin resolved by name runs whatever sits
/// beside `cargo_bin("clove")`. Publishing there makes that the fresh binary. As
/// a consequence one test run may use only one feature set per plugin; asking for
/// a second panics instead of replacing the binary another test is running.
pub fn plugin(package: &str, bin: &str, features: &str) -> &'static Path {
    built(package, bin, features, Placement::Workspace)
}

#[derive(Clone, Copy, PartialEq)]
enum Placement {
    Isolated,
    Workspace,
}

type Slot = &'static OnceLock<PathBuf>;

fn built(package: &str, bin: &str, features: &str, placement: Placement) -> &'static Path {
    let features = normalize_features(features);
    let slot = slot_for(package, bin, &features, placement);
    slot.get_or_init(|| build_and_publish(package, bin, &features, placement))
}

fn build_and_publish(package: &str, bin: &str, features: &str, placement: Placement) -> PathBuf {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let workspace_target = tmp.parent().expect("CARGO_TARGET_TMPDIR has a parent");
    let package_dir = tmp.join("escargot").join(package);
    let features_dir = if features.is_empty() {
        "default".to_owned()
    } else {
        features.replace(' ', "+")
    };
    let scratch = package_dir.join(&features_dir);
    let publish_dir = match placement {
        Placement::Isolated => scratch.join("bin"),
        Placement::Workspace => clove_bin_dir(),
    };
    for dir in [&package_dir, &publish_dir] {
        fs::create_dir_all(dir).unwrap_or_else(|err| panic!("create {}: {err}", dir.display()));
    }
    let published = publish_dir.join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));
    let stamp_path = publish_dir.join(format!(".{bin}.escargot-stamp"));

    // One lock per package/bin, whatever the features: a `Workspace` artifact
    // has one published path for every feature set.
    let lock_path = package_dir.join(format!("{bin}.lock"));
    let lock = fs::File::create(&lock_path)
        .unwrap_or_else(|err| panic!("create {}: {err}", lock_path.display()));
    lock.lock()
        .unwrap_or_else(|err| panic!("lock {}: {err}", lock_path.display()));

    let run_id = std::env::var("NEXTEST_RUN_ID").ok();
    if let Some(run_id) = &run_id {
        if let Ok(stamp) = fs::read_to_string(&stamp_path) {
            if let Some((stamp_run, stamp_features)) = stamp.split_once('\n') {
                if stamp_run == run_id && published.exists() {
                    if stamp_features == features {
                        return published;
                    }
                    if placement == Placement::Workspace {
                        panic!(
                            "{package}/{bin} is already built with [{stamp_features}] in this \
                             test run; rebuilding it with [{features}] would replace the \
                             shared binary"
                        );
                    }
                }
            }
        }
    }

    let mut build = escargot::CargoBuild::new()
        .package(package)
        .bin(bin)
        .target_dir(&scratch)
        // Share the workspace build dir, so a feature set is compiled once,
        // not once per scratch dir.
        .env("CARGO_BUILD_BUILD_DIR", workspace_target);
    if !features.is_empty() {
        build = build.features(features);
    }
    let reported = build
        .run()
        .unwrap_or_else(|err| panic!("build {package}/{bin} [{features}]: {err}"))
        .path()
        .to_path_buf();
    publish(&reported, &published);
    if let Some(run_id) = &run_id {
        fs::write(&stamp_path, format!("{run_id}\n{features}"))
            .unwrap_or_else(|err| panic!("write {}: {err}", stamp_path.display()));
    }
    drop(lock);
    published
}

/// Atomically replace `published` with `reported` unless the two already hold
/// the same bytes.
fn publish(reported: &Path, published: &Path) {
    let new = fs::read(reported).unwrap_or_else(|err| panic!("read {}: {err}", reported.display()));
    if fs::read(published).is_ok_and(|old| old == new) {
        return;
    }
    let dir = published.parent().expect("published binary has a parent");
    let name = published.file_name().expect("published binary has a name");
    let staged = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let _ = fs::remove_file(&staged);
    // A hard link writes no bytes from this process, so no file descriptor open
    // for writing can leak into a sibling thread's fork and make the later exec
    // fail with ETXTBSY. Copy only where linking is impossible.
    if fs::hard_link(reported, &staged).is_err() {
        fs::copy(reported, &staged).unwrap_or_else(|err| {
            panic!("copy {} to {}: {err}", reported.display(), staged.display())
        });
    }
    fs::rename(&staged, published).unwrap_or_else(|err| {
        panic!(
            "rename {} to {}: {err}",
            staged.display(),
            published.display()
        )
    });
}

/// The directory of the `clove` binary the tests run (`cargo_bin("clove")`).
/// nextest sets `CARGO_BIN_EXE_*` at run time; plain `cargo test` bakes it in.
fn clove_bin_dir() -> PathBuf {
    let clove = std::env::var_os("CARGO_BIN_EXE_clove")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_clove")));
    clove
        .parent()
        .expect("the clove binary has a parent dir")
        .to_path_buf()
}

/// The process-wide build slot for one artifact. A `Workspace` artifact shares
/// one output path across feature sets, so a second set is refused here.
fn slot_for(package: &str, bin: &str, features: &str, placement: Placement) -> Slot {
    static SLOTS: Mutex<BTreeMap<(String, String, String), Slot>> = Mutex::new(BTreeMap::new());
    let mut slots = SLOTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if placement == Placement::Workspace {
        if let Some((pkg, b, other)) = slots
            .keys()
            .find(|(pkg, b, other)| pkg == package && b == bin && other != features)
        {
            panic!(
                "{pkg}/{b} is already built with [{other}] in this process; \
                 rebuilding it with [{features}] would relink the shared binary"
            );
        }
    }
    slots
        .entry((package.to_owned(), bin.to_owned(), features.to_owned()))
        .or_insert_with(|| Box::leak(Box::new(OnceLock::new())))
}

/// `"web  test-knobs"` and `"test-knobs,web"` name the same build.
fn normalize_features(features: &str) -> String {
    let mut names: Vec<&str> = features
        .split([' ', ','])
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();
    names.join(" ")
}
