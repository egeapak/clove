//! Shared by the suites that spawn a real daemon.

/// An escargot build of `cloved` into this package's own scratch target dir.
///
/// Building into the workspace `target/<profile>/` would overwrite the
/// `cloved` that `crates/cloved/tests` run (`CARGO_BIN_EXE_cloved`) with
/// whatever features this build picked, so those suites would run against the
/// wrong binary depending on which of these tests built last.
#[cfg(unix)]
pub fn cloved_build() -> escargot::CargoBuild {
    escargot::CargoBuild::new()
        .package("cloved")
        .bin("cloved")
        .target_dir(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("cloved"))
}
