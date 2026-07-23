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

/// The host↔plugin contract version (`PLUGIN_REGISTRY.md` §2) — the single source
/// of truth for the plugin API version, shared by the host (which threads it into
/// `$CLOVE_PLUGIN_API` and compares it against a probed plugin's range) and every
/// plugin (which advertises it via `--clove-plugin-info`). Bumped only on a
/// breaking change to the env/argv/envelope contract; starts at `1`.
pub const CLOVE_PLUGIN_API: u32 = 1;

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

    /// True if this plugin advertises `capability` (a `"<mux>:<provider>"` or bare
    /// `"<command>"` token) in its [`provides`](Self::provides) set. A
    /// multi-capability plugin's `main` uses it (via [`unsupported_capability`]) to
    /// reject a probe-free structural dispatch (`PLUGIN_SYSTEM.md` §4.2) for a
    /// capability it does not implement.
    pub fn provides_capability(&self, capability: &str) -> bool {
        self.provides.contains(&capability)
    }

    /// The JSON blob emitted for the `--clove-plugin-info` probe.
    ///
    /// The authored `{ name, version, about, provides }` keys, plus the compat
    /// fields (`PLUGIN_REGISTRY.md` §2) auto-filled from compile-time constants so
    /// a plugin never has to hand-write (or drift on) them: `clove_plugin_api` /
    /// `min_clove_plugin_api` / `max_clove_plugin_api` all default to the
    /// [`CLOVE_PLUGIN_API`] the plugin was built against (v1: exact match; a plugin
    /// may widen the range in a later contract), and `max_schema` is the highest
    /// on-disk item schema this build understands.
    fn to_json(self) -> serde_json::Value {
        json!({
            "name": self.name,
            "version": self.version,
            "about": self.about,
            "provides": self.provides,
            "clove_plugin_api": CLOVE_PLUGIN_API,
            "min_clove_plugin_api": CLOVE_PLUGIN_API,
            "max_clove_plugin_api": CLOVE_PLUGIN_API,
            "max_schema": clove_types::CURRENT_SCHEMA_VERSION,
        })
    }
}

/// If argv carries `--clove-plugin-info`, print the canonical metadata JSON and
/// return `true` — the caller should then exit `0`.
///
/// This is the **single place** the `--clove-plugin-info` response is produced
/// (the authored `{name,version,about,provides}` plus the §2 compat fields,
/// auto-filled from compile-time constants). [`run_with_info`] uses it, and a
/// plugin that hand-rolls its `main` (for custom human rendering) calls it at the
/// top of `main` instead of re-authoring the JSON — so no plugin ever hand-writes
/// the info shape or drifts on the compat fields. The check runs before any env
/// materialization, so `clove plugin list` can probe without a repo context.
pub fn info_requested(info: &PluginInfo) -> bool {
    if std::env::args().skip(1).any(|arg| arg == INFO_FLAG) {
        println!("{}", info.to_json());
        true
    } else {
        false
    }
}

/// The standard clean-failure error for a multi-capability plugin handed a
/// capability it does not implement (`PLUGIN_SYSTEM.md` §4.2).
///
/// Because the host dispatches umbrella-fallback binaries *structurally* — by name,
/// probe-free, on the hot path — a binary reached via a fallback (e.g. the
/// import-only `clove-import-tk` reached for `clove export tk` via the cross-sibling
/// candidate) must reject the request itself. Its `main` matches on
/// [`PluginContext::command`] and, in the default arm, returns this error:
/// `UnsupportedCapability` maps to exit 2 with a distinct `UNSUPPORTED_CAPABILITY`
/// wire code, so the caller sees a clean, specific failure rather than a panic or a
/// misleading success.
pub fn unsupported_capability(info: &PluginInfo, cx: &PluginContext) -> CloveError {
    CloveError::UnsupportedCapability {
        plugin: info.name.to_owned(),
        capability: cx.capability(),
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
    if info_requested(&info) {
        return ExitCode::SUCCESS;
    }

    let argv: Vec<String> = std::env::args().skip(1).collect();

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
    fn provides_capability_matches_the_token_set() {
        let info = PluginInfo {
            name: "clove-sync-github",
            version: "0.1.0",
            about: "Two-way GitHub sync",
            provides: &["sync:github", "import:github", "export:github"],
        };
        assert!(info.provides_capability("import:github"));
        assert!(info.provides_capability("export:github"));
        assert!(info.provides_capability("sync:github"));
        assert!(!info.provides_capability("import:beads"));
        assert!(!info.provides_capability("sync"));
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
        // The compat fields (§2) are auto-filled from compile-time constants —
        // the plugin main never authors them.
        assert_eq!(value["clove_plugin_api"], CLOVE_PLUGIN_API);
        assert_eq!(value["min_clove_plugin_api"], CLOVE_PLUGIN_API);
        assert_eq!(value["max_clove_plugin_api"], CLOVE_PLUGIN_API);
        assert_eq!(value["max_schema"], clove_types::CURRENT_SCHEMA_VERSION);
    }
}
