//! `clove-echo` — a fixture plugin that echoes its forwarded argv and a slice of
//! its [`PluginContext`] back through the standard `clove-plugin` envelope.
//!
//! It exists to prove the host↔plugin seam end-to-end: an integration test runs
//! `clove echo …` (or `clove-echo` directly), and asserts the returned `data`
//! carries the argv the host forwarded and the `CLOVE_*` context it exported.

use clove_plugin::{run_with_info, PluginInfo};
use serde_json::json;

/// Metadata answered on `--clove-plugin-info` (PLUGIN_SYSTEM.md §7).
const INFO: PluginInfo = PluginInfo {
    name: "clove-echo",
    version: env!("CARGO_PKG_VERSION"),
    about: "Echo fixture: reflects argv + context through the envelope",
    provides: &["echo"],
};

fn main() -> std::process::ExitCode {
    run_with_info(INFO, |cx, args| {
        // The file name this binary was invoked as (e.g. `clove-sync-echo` when the
        // same fixture is installed under a second name), so a dispatch test can
        // observe *which* binary the umbrella fallback resolved.
        let binary = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        Ok(json!({
            "argv": args.args,
            "binary": binary,
            "provider": cx.provider,
            "command": cx.command,
            "clove_dir": cx.clove_dir.as_str(),
            "sync_dir": cx.sync_dir.as_str(),
            "config_path": cx.config_path.as_str(),
            "format": format!("{:?}", cx.format),
            "id_prefix": cx.id_prefix,
        }))
    })
}
