//! Guards the two invariants that make a Node-free `cargo install` serve the
//! real UI instead of the placeholder.
//!
//! Both failures are silent: the crate builds, the server starts, and the user
//! gets a "the SPA was not built" page. Neither is caught by an assertion on the
//! serving code, because the serving code is fine — the assets never arrived.

use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `include` must name `dist-gz/**`, or the git-ignored build output is left out
/// of the packaged `.crate` and every `cargo install` user gets the placeholder.
#[test]
fn cargo_toml_includes_the_built_assets() {
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    let include = manifest
        .lines()
        .find(|l| l.trim_start().starts_with("include"))
        .expect("clove-web must declare `include` so dist-gz/ reaches the .crate");
    assert!(
        include.contains("dist-gz"),
        "`include` must list dist-gz/** or the published crate ships no web UI; found: {include}"
    );
}

/// `build.rs` must leave a prebuilt `dist-gz/` alone when there are no `web/`
/// sources beside it. That is the packaged-crate layout, and regenerating there
/// mirrors a fresh placeholder over the real assets — which is what forced
/// `--no-verify` before the guard existed.
#[test]
fn build_script_preserves_prebuilt_assets() {
    let build_rs = std::fs::read_to_string(crate_dir().join("build.rs")).unwrap();
    assert!(
        build_rs.contains("is_prebuilt"),
        "build.rs must keep the prebuilt-dist-gz guard; without it, packaging \
         regenerates a placeholder over the included assets"
    );

    // The guard is only sound if it keys on something a placeholder never has.
    // `ensure_placeholder` writes a bare index.html, so `_app/` is the
    // discriminator; assert the two stay in step.
    assert!(
        build_rs.contains("_app"),
        "the prebuilt check must key on the hashed `_app` dir, which only a real \
         SvelteKit build produces"
    );
}

/// A real build leaves ~50 files under `dist-gz/`; the placeholder leaves one.
/// Skipped when the working tree has no build yet (a fresh clone, or CI running
/// with `CLOVE_SKIP_WEB_BUILD=1`) — the point is to catch a *silent* downgrade
/// in a tree that did build the SPA, not to force npm onto every test run.
#[test]
fn a_built_tree_has_more_than_the_placeholder() {
    let dist_gz = crate_dir().join("dist-gz");
    if !dist_gz.join("_app").is_dir() {
        eprintln!("skipping: no real SPA build in dist-gz/ (placeholder or absent)");
        return;
    }
    let count = count_files(&dist_gz);
    assert!(
        count > 1,
        "dist-gz/ has a real `_app` dir but only {count} file(s); the asset mirror \
         is truncated and the published crate would ship a broken UI"
    );
}

fn count_files(dir: &Path) -> usize {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(entry.path()),
                Ok(_) => total += 1,
                Err(_) => {}
            }
        }
    }
    total
}
