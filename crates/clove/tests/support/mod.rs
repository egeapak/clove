//! Shared by the suites that spawn a real daemon or a plugin binary.
//!
//! Every escargot build goes through [`cloved`] or [`plugin`], which build each
//! artifact at most once per test process. That is load-bearing, not a speed-up:
//! every `cargo build` — even a no-op one — deletes and re-hard-links the binary
//! it reports, so a test thread rebuilding an artifact makes it briefly vanish
//! under a sibling thread that is spawning or copying it (`ENOENT`).
// Each test binary uses a subset of these helpers.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// `cloved` built with `features` (space-separated; `""` for the defaults).
///
/// Each feature set gets its own artifact dir, so two suites asking for
/// different features never relink each other's binary, and none of them
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
/// beside `cargo_bin("clove")`. Building there makes that the fresh binary. As a
/// consequence one process may use only one feature set per plugin; asking for a
/// second panics instead of relinking the binary another test is running.
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
    slot.get_or_init(|| {
        let mut build = escargot::CargoBuild::new().package(package).bin(bin);
        if !features.is_empty() {
            build = build.features(&features);
        }
        if placement == Placement::Isolated {
            let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
            let workspace_target = tmp.parent().expect("CARGO_TARGET_TMPDIR has a parent");
            let features_dir = if features.is_empty() {
                "default".to_owned()
            } else {
                features.replace(' ', "+")
            };
            build = build
                .target_dir(tmp.join("escargot").join(package).join(features_dir))
                .env("CARGO_BUILD_BUILD_DIR", workspace_target);
        }
        build
            .run()
            .unwrap_or_else(|err| panic!("build {package}/{bin} [{features}]: {err}"))
            .path()
            .to_path_buf()
    })
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
