//! The Claude Code plugin manifest (`.claude-plugin/`) ships from this repo, so
//! its version must move with the workspace version on every release.

use std::fs;
use std::path::{Path, PathBuf};

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
fn plugin_version_matches_workspace_version() {
    let plugin = read_json(".claude-plugin/plugin.json");
    assert_eq!(
        plugin["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION")),
        ".claude-plugin/plugin.json `version` must equal [workspace.package] version"
    );
}

#[test]
fn marketplace_lists_the_plugin_without_its_own_version() {
    let marketplace = read_json(".claude-plugin/marketplace.json");
    let plugins = marketplace["plugins"]
        .as_array()
        .expect("marketplace has a plugins list");
    let clove = plugins
        .iter()
        .find(|entry| entry["name"] == "clove")
        .expect("marketplace lists the clove plugin");
    assert_eq!(clove["source"], "./");
    // A version here would override plugin.json's and could silently drift.
    assert!(
        clove.get("version").is_none(),
        "keep the version in plugin.json only"
    );
}
