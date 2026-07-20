//! The `run` harness (`PLUGIN_SYSTEM.md` §6.3/§6.5) — the envelope + exit-code
//! wrapper that makes a plugin `main` a thin shell around its provider logic.
//!
//! [`run_with_info`] materializes a [`PluginContext`] from the environment,
//! parses argv into [`PluginArgs`], answers the `--clove-plugin-info` metadata
//! probe (§7), invokes the closure, and renders the result (or a [`CloveError`])
//! as the correct envelope for `cx.format`, returning the matching
//! [`std::process::ExitCode`].

use std::process::ExitCode;

use serde_json::json;

use clove_types::CloveError;

use crate::context::PluginContext;
use crate::envelope::{emit_error, emit_success};

/// The `--clove-plugin-info` metadata probe token (§7). A plugin answers it with
/// a small JSON blob so `clove plugin list` can describe it without a full run.
const INFO_FLAG: &str = "--clove-plugin-info";

/// The forwarded tail args — everything after the cargo-style leading
/// command/provider echo (`PLUGIN_SYSTEM.md` §6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginArgs {
    /// The raw forwarded args (the multiplexer conventions like `--dry-run` /
    /// `--format` live here for the plugin to parse).
    pub args: Vec<String>,
}

impl PluginArgs {
    /// Build the tail args from a raw argv (already excluding argv[0]).
    ///
    /// The host invokes a plugin cargo-style, echoing the path taken as leading
    /// args: `clove-sync-github sync github <rest…>`. Strip a leading token that
    /// matches `command`, then one that matches `provider` (if any), so the same
    /// binary also works when invoked directly (`clove-sync-github egeapak/clove`)
    /// where those leading echoes are absent.
    pub fn from_argv(argv: &[String], command: &str, provider: Option<&str>) -> PluginArgs {
        let mut rest = argv;
        if rest.first().is_some_and(|first| first == command) {
            rest = &rest[1..];
            if let Some(provider) = provider {
                if rest.first().is_some_and(|first| first == provider) {
                    rest = &rest[1..];
                }
            }
        }
        PluginArgs {
            args: rest.to_vec(),
        }
    }
}

/// Static metadata a plugin advertises via `--clove-plugin-info` (§7).
#[derive(Debug, Clone, Copy)]
pub struct PluginInfo {
    /// The plugin binary name, e.g. `clove-sync-github`.
    pub name: &'static str,
    /// The plugin semver.
    pub version: &'static str,
    /// A one-line description for `clove plugin list`.
    pub about: &'static str,
    /// The dispatch paths this plugin provides, e.g. `["sync:github"]`.
    pub provides: &'static [&'static str],
}

impl PluginInfo {
    /// The empty metadata used by [`run`] (a plugin that opts out of the
    /// `--clove-plugin-info` protocol).
    const EMPTY: PluginInfo = PluginInfo {
        name: "",
        version: "",
        about: "",
        provides: &[],
    };

    /// The `{ name, version, about, provides }` JSON blob emitted for the probe.
    fn to_json(self) -> serde_json::Value {
        json!({
            "name": self.name,
            "version": self.version,
            "about": self.about,
            "provides": self.provides,
        })
    }
}

/// Run a plugin without metadata (no `--clove-plugin-info` support).
///
/// See [`run_with_info`] for the full behavior; this is the same harness with an
/// empty [`PluginInfo`].
pub fn run<F>(f: F) -> ExitCode
where
    F: FnOnce(&PluginContext, PluginArgs) -> Result<serde_json::Value, CloveError>,
{
    run_with_info(PluginInfo::EMPTY, f)
}

/// Run a plugin, answering the `--clove-plugin-info` probe with `info`.
///
/// Behavior:
/// 1. If argv contains `--clove-plugin-info`, print `info` as JSON and exit `0`
///    (this runs *before* env materialization, so `clove plugin list` can probe
///    a plugin without a repo context).
/// 2. Materialize [`PluginContext::from_env`]. On failure, render the error
///    envelope using `CLOVE_FORMAT` read directly (the typed context isn't
///    available yet) and return the validation exit code.
/// 3. Invoke `f`, then render its `Ok(data)` as a success envelope or its
///    `Err(e)` as an error envelope, honoring `cx.format` and `cx.quiet`, and
///    return the matching [`ExitCode`].
pub fn run_with_info<F>(info: PluginInfo, f: F) -> ExitCode
where
    F: FnOnce(&PluginContext, PluginArgs) -> Result<serde_json::Value, CloveError>,
{
    let argv: Vec<String> = std::env::args().skip(1).collect();

    if argv.iter().any(|arg| arg == INFO_FLAG) {
        println!("{}", info.to_json());
        return ExitCode::SUCCESS;
    }

    let cx = match PluginContext::from_env() {
        Ok(cx) => cx,
        Err(env_err) => {
            // The typed context is unavailable, so read the format/quiet hints
            // directly for this early-failure path (§6.5).
            let format = std::env::var("CLOVE_FORMAT")
                .ok()
                .and_then(|value| clove_core::OutputFormat::parse(&value))
                .unwrap_or_default();
            let quiet = std::env::var("CLOVE_QUIET").is_ok_and(|value| value == "1");
            let err: CloveError = env_err.into();
            return ExitCode::from(emit_error(format, &err, quiet));
        }
    };

    let args = PluginArgs::from_argv(&argv, &cx.command, cx.provider.as_deref());

    match f(&cx, args) {
        Ok(data) => {
            emit_success(cx.format, data);
            ExitCode::SUCCESS
        }
        Err(err) => ExitCode::from(emit_error(cx.format, &err, cx.quiet)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn strips_command_and_provider_echo() {
        let argv = s(&["sync", "github", "egeapak/clove", "--dry-run"]);
        let args = PluginArgs::from_argv(&argv, "sync", Some("github"));
        assert_eq!(args.args, s(&["egeapak/clove", "--dry-run"]));
    }

    #[test]
    fn strips_only_command_for_generic_plugin() {
        let argv = s(&["frobnicate", "--wibble"]);
        let args = PluginArgs::from_argv(&argv, "frobnicate", None);
        assert_eq!(args.args, s(&["--wibble"]));
    }

    #[test]
    fn keeps_argv_when_leading_echo_absent() {
        // Invoked directly: `clove-sync-github egeapak/clove` (no echo).
        let argv = s(&["egeapak/clove"]);
        let args = PluginArgs::from_argv(&argv, "sync", Some("github"));
        assert_eq!(args.args, s(&["egeapak/clove"]));
    }

    #[test]
    fn provider_not_stripped_without_matching_command() {
        // Provider token alone (no leading command) is left untouched.
        let argv = s(&["github", "egeapak/clove"]);
        let args = PluginArgs::from_argv(&argv, "sync", Some("github"));
        assert_eq!(args.args, s(&["github", "egeapak/clove"]));
    }

    #[test]
    fn info_serializes_expected_shape() {
        let info = PluginInfo {
            name: "clove-sync-github",
            version: "0.1.0",
            about: "Two-way GitHub sync",
            provides: &["sync:github"],
        };
        let value = info.to_json();
        assert_eq!(value["name"], "clove-sync-github");
        assert_eq!(value["version"], "0.1.0");
        assert_eq!(value["about"], "Two-way GitHub sync");
        assert_eq!(value["provides"][0], "sync:github");
    }
}
