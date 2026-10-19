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
        // Process-model probes for crates/clove/tests/plugin_dispatch.rs: exit
        // with a given code, or record the pid and block until a signal ends the
        // process (default dispositions, so the test sees how the host relays).
        if let Some(code) = flag_value(&args.args, "--exit-code") {
            std::process::exit(code.parse().expect("--exit-code takes an integer"));
        }
        if let Some(pid_file) = flag_value(&args.args, "--wait-for-signal") {
            std::fs::write(pid_file, std::process::id().to_string()).expect("write the pid file");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
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

/// The value after `flag` in the forwarded args, if present.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).map(String::as_str)
}
