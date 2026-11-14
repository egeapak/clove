//! The workspace version is written in several files, and a release must move
//! all of them together (`docs/RELEASE.md` §8). `claude_plugin.rs` pins
//! `.claude-plugin/plugin.json`; this pins the rest.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

fn read_json(relative: &str) -> serde_json::Value {
    let path = workspace_root().join(relative);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn web_package_version_matches_workspace_version() {
    let package = read_json("crates/clove-web/web/package.json");
    assert_eq!(
        package["version"].as_str(),
        Some(VERSION),
        "crates/clove-web/web/package.json `version` must equal [workspace.package] version"
    );
    let lock = read_json("crates/clove-web/web/package-lock.json");
    for (what, value) in [
        ("version", &lock["version"]),
        ("packages[\"\"].version", &lock["packages"][""]["version"]),
    ] {
        assert_eq!(
            value.as_str(),
            Some(VERSION),
            "crates/clove-web/web/package-lock.json `{what}` must equal [workspace.package] \
             version (run `npm version {VERSION} --no-git-tag-version` in crates/clove-web/web)"
        );
    }
}

/// Every dependency of one workspace crate on another that names a version
/// (the `[workspace.dependencies]` entries do) names the workspace version.
/// `cargo publish` writes that requirement into the published crate, so a stale
/// one makes the new crates depend on the previous release.
#[test]
fn internal_dependency_versions_match_workspace_version() {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    let members: Vec<&str> = packages
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();

    let mut checked = 0;
    for package in packages {
        assert_eq!(
            package["version"].as_str(),
            Some(VERSION),
            "{} does not inherit the workspace version",
            package["name"]
        );
        for dep in package["dependencies"].as_array().unwrap() {
            let name = dep["name"].as_str().unwrap();
            let req = dep["req"].as_str().unwrap();
            // A path-only dependency has the requirement `*`.
            if dep["path"].is_null() || !members.contains(&name) || req == "*" {
                continue;
            }
            assert_eq!(
                req,
                format!("^{VERSION}"),
                "{} depends on {name} `{req}`; set `version = \"{VERSION}\"` for {name} under \
                 [workspace.dependencies] in the root Cargo.toml",
                package["name"]
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no versioned internal dependency was checked");
}

/// The first `key = "value"` line of a TOML file, without a TOML parser.
fn toml_string(relative: &str, key: &str) -> String {
    let text = fs::read_to_string(workspace_root().join(relative)).unwrap();
    let prefix = format!("{key} = \"");
    text.lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{relative} has no `{key}` line"))
        .to_owned()
}

/// Every toolchain that the workflows install is the pinned version of
/// `rust-toolchain.toml`, the minimum version of the root `Cargo.toml`, or
/// nightly (cargo-fuzz). A workflow that installs another version builds with a
/// compiler that nothing else uses.
#[test]
fn workflow_toolchains_match_the_pin_and_the_minimum_version() {
    let pin = toml_string("rust-toolchain.toml", "channel");
    assert_eq!(
        pin.split('.').count(),
        3,
        "rust-toolchain.toml must pin a full version, found {pin}"
    );
    let minimum = toml_string("Cargo.toml", "rust-version");
    let minimum_full = format!("{minimum}.0");
    assert_eq!(
        VERSION_OF_THIS_PACKAGE_MSRV, minimum,
        "xtask must inherit the workspace rust-version"
    );

    let mut pinned = 0;
    let mut minimums = 0;
    for file in ["ci.yml", "release.yml", "advisories.yml"] {
        let relative = format!(".github/workflows/{file}");
        let text = fs::read_to_string(workspace_root().join(&relative)).unwrap();
        for line in text.lines() {
            let Some(value) = line.trim().strip_prefix("toolchain: ") else {
                continue;
            };
            if value == pin {
                pinned += 1;
            } else if value == minimum_full {
                minimums += 1;
            } else {
                assert_eq!(value, "nightly", "{relative} installs toolchain {value}");
            }
        }
        for token in text.split_whitespace() {
            if let Some(version) = token.strip_prefix('+') {
                if version.starts_with(|c: char| c.is_ascii_digit()) {
                    assert_eq!(version, minimum_full, "{relative} runs cargo +{version}");
                }
            }
        }
    }
    assert!(pinned > 0, "no workflow installs the pinned toolchain");
    assert!(minimums > 0, "no workflow installs the minimum version");
}

const VERSION_OF_THIS_PACKAGE_MSRV: &str = env!("CARGO_PKG_RUST_VERSION");
