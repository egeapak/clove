//! Every published crate must carry its license text in its `.crate` package.
//!
//! clove is AGPL-3.0-only (`LICENSE` + `LICENSE-PLUGIN-EXCEPTION`), except the
//! plugin SDK crates, which are MIT OR Apache-2.0 (`LICENSE-MIT` +
//! `LICENSE-APACHE`). cargo packages only the files inside a crate's own
//! directory, so each crate keeps real copies of the root files (not symlinks:
//! Windows checkouts may not materialize them). This test pins that each copy
//! exists, is byte-identical to the root file, survives an `include` list, and
//! matches the crate's declared `license`.

use std::fs;
use std::path::{Path, PathBuf};

const AGPL: &str = "AGPL-3.0-only";
const SDK: &str = "MIT OR Apache-2.0";
const AGPL_FILES: [&str; 2] = ["LICENSE", "LICENSE-PLUGIN-EXCEPTION"];
const SDK_FILES: [&str; 2] = ["LICENSE-MIT", "LICENSE-APACHE"];

/// The crates that form the permissively licensed plugin SDK.
const SDK_CRATES: [&str; 3] = ["clove-types", "clove-plugin", "clove-plugin-echo"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

/// The value of a top-level `key = "value"` line in a manifest, if present.
fn manifest_value(manifest: &str, key: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.trim_start().strip_prefix('=')?;
        Some(rest.trim().trim_matches('"').to_owned())
    })
}

fn package_name(manifest: &str) -> String {
    manifest_value(manifest, "name").expect("every crate manifest has a name")
}

#[test]
fn workspace_default_license_is_agpl() {
    let root = fs::read_to_string(workspace_root().join("Cargo.toml")).unwrap();
    assert!(
        root.contains(&format!("license = \"{AGPL}\"")),
        "the [workspace.package] license must be {AGPL}"
    );
}

#[test]
fn every_published_crate_ships_its_license_files() {
    let root = workspace_root();
    let mut checked = 0;
    for entry in fs::read_dir(root.join("crates")).unwrap() {
        let dir = entry.unwrap().path();
        let Ok(manifest) = fs::read_to_string(dir.join("Cargo.toml")) else {
            continue;
        };
        let name = package_name(&manifest);
        let is_sdk = SDK_CRATES.contains(&name.as_str());

        // The declared license matches the crate's side of the split.
        if is_sdk {
            assert_eq!(
                manifest_value(&manifest, "license").as_deref(),
                Some(SDK),
                "{name} is an SDK crate and must declare `license = \"{SDK}\"`"
            );
        } else {
            assert!(
                manifest.contains("license.workspace = true"),
                "{name} must inherit the workspace license ({AGPL})"
            );
        }

        if manifest.lines().any(|l| l.trim() == "publish = false") {
            continue;
        }
        let files = if is_sdk { SDK_FILES } else { AGPL_FILES };
        let include = manifest_value(&manifest, "include");
        for file in files {
            let copy = dir.join(file);
            let bytes = fs::read(&copy)
                .unwrap_or_else(|e| panic!("{name}: missing {}: {e}", copy.display()));
            assert_eq!(
                bytes,
                fs::read(root.join(file)).unwrap(),
                "{name}: {file} differs from the workspace root copy"
            );
            if let Some(include) = &include {
                // Anchored: a bare `"LICENSE"` also matches every nested
                // LICENSE, such as git-ignored `node_modules/**/LICENSE`.
                assert!(
                    include.contains(&format!("\"/{file}\"")),
                    "{name}: its `include` list must name /{file} (anchored)"
                );
            }
        }
        checked += 1;
    }
    assert!(
        checked >= 10,
        "only {checked} crates checked; did the layout move?"
    );
}
