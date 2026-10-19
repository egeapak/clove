//! `clove-echo` — a fixture plugin that echoes its forwarded argv and a slice of
//! its [`PluginContext`] back through the standard `clove-plugin` envelope.
//!
//! It exists to prove the host↔plugin seam end-to-end: an integration test runs
//! `clove echo …` (or `clove-echo` directly), and asserts the returned `data`
//! carries the argv the host forwarded and the `CLOVE_*` context it exported.

use clove_plugin::tool::{EditArgs, IdArgs, ListArgs, NewArgs};
use clove_plugin::{run_with_info, CloveError, PluginContext, PluginInfo};
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
        if args.args.iter().any(|a| a == "--host-roundtrip") {
            return host_roundtrip(cx);
        }
        if args.args.iter().any(|a| a == "--host-bad-token") {
            // Authenticate with a wrong token: the host must refuse.
            let socket = cx.host_socket.clone().unwrap_or_default();
            let refused = clove_plugin::HostClient::connect(&socket, "00", "clove-echo", None);
            return Ok(json!({ "refused": refused.err().map(|e| e.to_string()) }));
        }
        if args.args.iter().any(|a| a == "--host-oversize") {
            // One request line over the host's 16 MiB cap.
            let mut host = cx.host()?;
            let huge = "x".repeat(17 * 1024 * 1024);
            let result = host.call("search", json!({ "text": huge }));
            let after = host.call("config", json!({}));
            return Ok(json!({
                "oversize": result.err().map(|e| e.to_string()),
                "after": after.err().map(|e| e.to_string()),
            }));
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

/// Drive the host IPC channel end to end (PLUGIN_SYSTEM.md §6.6): create an
/// item, read it back, edit it, list, and provoke one tool error, reflecting
/// every answer so the dispatch test can assert on them.
fn host_roundtrip(cx: &PluginContext) -> Result<serde_json::Value, CloveError> {
    let mut host = cx.host()?;
    let info = host.info().clone();
    let created = host.create(&NewArgs {
        title: "Made over the host channel".to_owned(),
        item_type: Some("bug".to_owned()),
        priority: Some(1),
        labels: Some(vec!["ipc".to_owned()]),
        deps: None,
        parent: None,
        assignee: None,
        body: Some("body text".to_owned()),
    })?;
    let id = created["id"].as_str().unwrap_or_default().to_owned();
    let shown = host.show(&IdArgs {
        id: id.clone(),
        shape: Default::default(),
    })?;
    let edited = host.edit(&EditArgs {
        id: id.clone(),
        status: Some("in_progress".to_owned()),
        priority: None,
        item_type: None,
        title: None,
        assignee: None,
        body: None,
        add_labels: None,
        remove_labels: None,
    })?;
    let listed = host.list(&ListArgs::default())?;
    let missing_code = host
        .show(&IdArgs {
            id: format!("{}-ZZZZZZZZ", cx.id_prefix),
            shape: Default::default(),
        })
        .err()
        .map(|e| match e {
            clove_plugin::HostError::Tool {
                clove_code, exit, ..
            } => json!({ "clove_code": clove_code, "exit": exit }),
            other => json!({ "other": other.to_string() }),
        });
    Ok(json!({
        "hello": {
            "host_api": info.host_api,
            "clove_dir": info.clove_dir,
            "methods": info.methods,
        },
        "created": created,
        "shown": shown,
        "edited": edited,
        "listed": listed,
        "missing_code": missing_code,
    }))
}
