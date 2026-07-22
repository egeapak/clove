//! Cargo-style external-subcommand plugin dispatch (`PLUGIN_SYSTEM.md` §4–§7).
//!
//! This is the host half of the plugin seam: a bare `clove <x>` that matches no
//! built-in resolves a `clove-<x>` binary on the search path and hands off to it,
//! exactly as `cargo foo` runs `cargo-foo`. This module owns the three pure,
//! always-compiled pieces:
//!
//! - **name construction** ([`binary_name`]) and **path search**
//!   ([`resolve`] / [`list`]) — a `stat`-only walk (no exec, no spawn) so
//!   `clove plugin list` can enumerate cheaply;
//! - the **environment contract** ([`export_env`]) — every `CLOVE_*` var the
//!   plugin-side `clove_plugin::PluginContext::from_env` reads, written from one
//!   place (§6.2);
//! - the **invocation** ([`run_plugin`]) — `exec` on Unix (the host replaces its
//!   own image so the plugin gets the real tty and unbuffered stdio),
//!   spawn-and-wait propagating the exit code on Windows (§6.1).

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};
use clove_core::OutputFormat;
use clove_types::CloveError;

use crate::cli::ColorChoice;
use crate::context::Ctx;
use crate::exit::ExitCode;

/// The host↔plugin contract version (`$CLOVE_PLUGIN_API`, §6.2). Single-sourced
/// from `clove-plugin` so the host and every plugin can never drift: the same
/// constant is threaded into the plugin's env here and advertised back by the
/// plugin's `--clove-plugin-info`, and the enriched `plugin list` compares them.
use clove_plugin::CLOVE_PLUGIN_API as PLUGIN_API_VERSION;

/// The already-resolved global behavior flags the host threads into a plugin's
/// environment (§6.2). These are the collapsed *effective* values (flag > env >
/// config), mirroring the fields on `context::Ctx` that live on `Cli` instead.
pub struct PluginGlobals {
    /// The resolved output envelope (`$CLOVE_FORMAT`).
    pub format: OutputFormat,
    /// The resolved color preference (`$CLOVE_COLOR`).
    pub color: ColorChoice,
    /// The `--quiet` flag (`$CLOVE_QUIET`).
    pub quiet: bool,
    /// The `--no-index` flag (`$CLOVE_NO_INDEX`).
    pub no_index: bool,
    /// The `--deep` flag (`$CLOVE_DEEP`).
    pub deep: bool,
}

/// A resolvable plugin: its subcommand name (the `clove-` prefix and executable
/// suffix stripped) and the absolute path that would be exec'd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    /// The subcommand name, e.g. `sync-github` for `clove-sync-github`.
    pub name: String,
    /// The resolved path to the plugin binary.
    pub path: Utf8PathBuf,
}

/// The candidate binary name for a dispatch path, e.g. `["sync", "github"]` →
/// `clove-sync-github` (plus the platform executable suffix, `.exe` on Windows).
fn binary_name(segments: &[&str]) -> String {
    format!(
        "clove-{}{}",
        segments.join("-"),
        std::env::consts::EXE_SUFFIX
    )
}

/// The ordered plugin search path (§5): the directory of the running `clove`
/// binary, then each dir in `$CLOVE_PLUGIN_PATH`, then each dir on `$PATH`.
///
/// The current-exe directory comes first so a plugin installed next to `clove`
/// (the common `cargo install` case) is found even when that dir is not on
/// `$PATH`. Splitting uses the platform path separator (`;` on Windows else `:`).
fn search_dirs() -> Vec<Utf8PathBuf> {
    let mut dirs: Vec<Utf8PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if let Ok(dir) = Utf8PathBuf::from_path_buf(parent.to_owned()) {
                dirs.push(dir);
            }
        }
    }

    for var in ["CLOVE_PLUGIN_PATH", "PATH"] {
        if let Some(value) = std::env::var_os(var) {
            for path in std::env::split_paths(&value) {
                if let Ok(dir) = Utf8PathBuf::from_path_buf(path) {
                    dirs.push(dir);
                }
            }
        }
    }

    dirs
}

/// Is `path` an existing regular file that can be executed? On Unix this requires
/// any execute bit set (`mode & 0o111`); on other platforms a plain file check.
fn is_executable(path: &Utf8Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        // Known gap (PLUGIN_SYSTEM.md §5): Windows should also match `PATHEXT`
        // (`.cmd`/`.bat`/`.ps1`), not just `EXE_SUFFIX`. Deferred — the CI target
        // is Unix; a plugin shipped as a non-`.exe` script is not yet discovered.
        true
    }
}

/// A dispatch segment must be a single, non-empty path component — no separators
/// and no `..`. This stops a subcommand token like `foo/../../bin/sh` from
/// resolving to an arbitrary path via traversal (git and cargo likewise forbid
/// path separators in subcommand names). Rejected here so *every* caller (the
/// generic and the provider path) is protected centrally.
fn is_valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != ".."
        && !segment.contains('/')
        && !segment.contains('\\')
        && !segment.contains(std::path::MAIN_SEPARATOR)
}

/// Resolve a plugin binary for a dispatch path, returning the first existing
/// executable found along the §5 search path (no exec, no spawn — pure `stat`).
///
/// Returns `None` for a segment that is not a single path component (§5), so a
/// traversal token can never name a binary outside a search dir.
pub fn resolve(segments: &[&str]) -> Option<Utf8PathBuf> {
    if !segments.iter().all(|s| is_valid_segment(s)) {
        return None;
    }
    let name = binary_name(segments);
    for dir in search_dirs() {
        let candidate = dir.join(&name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Enumerate every resolvable `clove-<x>` plugin along the §5 search path.
///
/// Files are matched on the `clove-<x>` name shape and executability, deduped by
/// name (first match wins, mirroring [`resolve`]'s precedence), and returned
/// sorted by name. The host's own adjacent binaries (`clove`, `cloved`) never
/// carry the `clove-` prefix and so are excluded automatically.
pub fn list() -> Vec<PluginInfo> {
    let prefix = "clove-";
    let suffix = std::env::consts::EXE_SUFFIX;

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut plugins: Vec<PluginInfo> = Vec::new();

    for dir in search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            // The host's own binaries are never plugins (defensive; they lack the
            // `clove-` prefix anyway).
            if file_name == "clove" || file_name == "cloved" {
                continue;
            }
            let Some(rest) = file_name.strip_prefix(prefix) else {
                continue;
            };
            let name = if suffix.is_empty() {
                rest
            } else {
                match rest.strip_suffix(suffix) {
                    Some(name) => name,
                    None => continue,
                }
            };
            if name.is_empty() {
                continue;
            }
            let path = dir.join(&file_name);
            if !is_executable(&path) {
                continue;
            }
            if seen.insert(name.to_owned()) {
                plugins.push(PluginInfo {
                    name: name.to_owned(),
                    path,
                });
            }
        }
    }

    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    plugins
}

/// The metadata a plugin advertises via `--clove-plugin-info`
/// (`PLUGIN_REGISTRY.md` §2/§3), as parsed by the host from its JSON reply.
///
/// The compat fields (`clove_plugin_api` / `min_clove_plugin_api` /
/// `max_clove_plugin_api`) are auto-filled by the `clove-plugin` harness from the
/// build's [`clove_plugin::CLOVE_PLUGIN_API`]; a legacy plugin that answers the
/// probe but omits them is treated as declaring the host's own contract version
/// (Phase 1 is entirely API v1), so it lists as compatible rather than unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedInfo {
    /// The plugin semver (`version`).
    pub version: String,
    /// The one-line description (`about`).
    pub about: String,
    /// The dispatch tokens the plugin provides, e.g. `["sync:github"]`.
    pub provides: Vec<String>,
    /// The contract version the plugin was built against.
    pub clove_plugin_api: u32,
    /// The lowest host contract version the plugin tolerates.
    pub min_clove_plugin_api: u32,
    /// The highest host contract version the plugin tolerates.
    pub max_clove_plugin_api: u32,
    /// The highest on-disk item schema the plugin understands.
    pub max_schema: u32,
}

/// The host↔plugin compatibility verdict for the enriched `plugin list`
/// (`PLUGIN_REGISTRY.md` §2). Computed by comparing the host's
/// [`clove_plugin::CLOVE_PLUGIN_API`] to the plugin's advertised `[min, max]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStatus {
    /// `min ≤ host ≤ max` — compatible.
    Ok,
    /// `host > max` — the plugin predates this clove; it still runs, with a warning.
    Outdated,
    /// `host < min` — the plugin needs a newer clove; dispatch would refuse.
    NeedsNewerClove,
    /// The probe failed (spawn error, non-zero exit, unparseable, or timeout) —
    /// a legacy/opaque plugin, still listed and run from the name heuristic.
    NoInfo,
}

impl PluginStatus {
    /// The wire spelling used in the JSON `status` field (§3).
    pub fn as_str(self) -> &'static str {
        match self {
            PluginStatus::Ok => "ok",
            PluginStatus::Outdated => "outdated",
            PluginStatus::NeedsNewerClove => "needs_newer_clove",
            PluginStatus::NoInfo => "no_info",
        }
    }

    /// Classify a probed plugin's `[min, max]` range against the host contract.
    fn classify(probed: &ProbedInfo) -> PluginStatus {
        let host = PLUGIN_API_VERSION;
        if host > probed.max_clove_plugin_api {
            PluginStatus::Outdated
        } else if host < probed.min_clove_plugin_api {
            PluginStatus::NeedsNewerClove
        } else {
            PluginStatus::Ok
        }
    }
}

/// An installed plugin enriched with the result of its `--clove-plugin-info` probe
/// (`PLUGIN_REGISTRY.md` §3): the resolvable [`PluginInfo`], the parsed metadata
/// (when the probe answered), the compat [`PluginStatus`], and the human-readable
/// `clove …` command(s) it provides.
#[derive(Debug, Clone)]
pub struct EnrichedPlugin {
    /// The resolvable binary (name + path) from the pure `stat` walk.
    pub info: PluginInfo,
    /// The parsed `--clove-plugin-info` metadata, or `None` when the probe failed.
    pub probed: Option<ProbedInfo>,
    /// The host↔plugin compatibility verdict.
    pub status: PluginStatus,
    /// The `clove …` invocation(s) this plugin answers, e.g.
    /// `["clove sync github"]` (from `provides`, or a name heuristic).
    pub commands: Vec<String>,
}

/// How long the host waits for a plugin to answer `--clove-plugin-info` before
/// giving up (killing the child and reporting `no_info`). Kept short so a hung or
/// misbehaving plugin can never wedge `plugin list` / `<mux> --help`.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// The poll interval while waiting for the probe child to exit.
const PROBE_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Probe a plugin binary for its `--clove-plugin-info` metadata (§3).
///
/// Spawns `<path> --clove-plugin-info`, captures stdout, and parses the JSON.
/// Bounded by a dependency-free timeout: the child is polled with `try_wait()`
/// and, if it has not exited within [`PROBE_TIMEOUT`], killed. Returns `None` on
/// spawn error, non-zero exit, unparseable output, or timeout — every failure
/// path collapses to "no metadata" so the caller lists the plugin from its name.
pub fn probe_info(path: &Utf8Path) -> Option<ProbedInfo> {
    use std::io::Read;
    use std::process::Stdio;

    let mut child = Command::new(path.as_std_path())
        .arg("--clove-plugin-info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= PROBE_TIMEOUT {
                    // Hung/slow plugin: kill it and give up (treat as no_info).
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(PROBE_POLL);
            }
            Err(_) => return None,
        }
    };

    if !status.success() {
        return None;
    }

    let mut stdout = String::new();
    child.stdout.take()?.read_to_string(&mut stdout).ok()?;

    parse_probe_json(&stdout)
}

/// Parse a plugin's `--clove-plugin-info` JSON into a [`ProbedInfo`] (§2).
///
/// A missing compat field defaults to the host contract version (a legacy plugin
/// that answers the probe but predates §2 is treated as API-compatible in the
/// all-v1 Phase 1). Split out from [`probe_info`] so it is unit-testable without
/// spawning a process.
fn parse_probe_json(stdout: &str) -> Option<ProbedInfo> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;

    let version = value["version"].as_str().unwrap_or_default().to_owned();
    let about = value["about"].as_str().unwrap_or_default().to_owned();
    let provides = value["provides"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let api = value["clove_plugin_api"]
        .as_u64()
        .map(|v| v as u32)
        .unwrap_or(PLUGIN_API_VERSION);
    let min = value["min_clove_plugin_api"]
        .as_u64()
        .map(|v| v as u32)
        .unwrap_or(api);
    let max = value["max_clove_plugin_api"]
        .as_u64()
        .map(|v| v as u32)
        .unwrap_or(api);
    let max_schema = value["max_schema"]
        .as_u64()
        .map(|v| v as u32)
        .unwrap_or(clove_types::CURRENT_SCHEMA_VERSION);

    Some(ProbedInfo {
        version,
        about,
        provides,
        clove_plugin_api: api,
        min_clove_plugin_api: min,
        max_clove_plugin_api: max,
        max_schema,
    })
}

/// Enumerate installed plugins ([`list`]) and enrich each with its
/// `--clove-plugin-info` probe (§3): the parsed metadata, the compat status, and
/// the `clove …` command(s) it provides. Needs no repository.
pub fn list_enriched() -> Vec<EnrichedPlugin> {
    list()
        .into_iter()
        .map(|info| {
            let probed = probe_info(&info.path);
            let status = match &probed {
                Some(p) => PluginStatus::classify(p),
                None => PluginStatus::NoInfo,
            };
            let provides = probed
                .as_ref()
                .map(|p| p.provides.clone())
                .unwrap_or_default();
            let commands = run_as(&provides, &info.name);
            EnrichedPlugin {
                info,
                probed,
                status,
                commands,
            }
        })
        .collect()
}

/// Map a plugin's `provides` tokens to the `clove …` command line(s) that reach it
/// (§3). A `"<mux>:<provider>"` token becomes `"clove <mux> <provider>"`. With no
/// `provides` (a legacy plugin), fall back to the binary-name heuristic: a
/// `sync-`/`import-`/`export-` prefix splits into `"clove <mux> <rest>"`, anything
/// else is a generic `"clove <name>"`.
pub fn run_as(provides: &[String], name: &str) -> Vec<String> {
    if !provides.is_empty() {
        return provides
            .iter()
            .map(|token| match token.split_once(':') {
                Some((mux, provider)) => format!("clove {mux} {provider}"),
                None => format!("clove {token}"),
            })
            .collect();
    }

    for mux in ["sync", "import", "export"] {
        if let Some(rest) = name.strip_prefix(&format!("{mux}-")) {
            if !rest.is_empty() {
                return vec![format!("clove {mux} {rest}")];
            }
        }
    }
    vec![format!("clove {name}")]
}

/// The wire spelling of a [`ColorChoice`] (`$CLOVE_COLOR`, §6.2).
fn color_wire(color: ColorChoice) -> &'static str {
    match color {
        ColorChoice::Auto => "auto",
        ColorChoice::Always => "always",
        ColorChoice::Never => "never",
    }
}

/// The wire spelling of a boolean var (exactly `0` / `1`, §6.2).
fn bool_wire(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

/// Export the full §6.2 environment onto `cmd`.
///
/// Every value is the resolved one the host already computed (repo discovery,
/// config load, format precedence), so the plugin re-derives nothing and can
/// never disagree with the host. This is the producer side of the contract read
/// back by `clove_plugin::PluginContext::from_env`; the two are pinned together
/// end-to-end by `tests/plugin_dispatch.rs` (the `clove-echo` fixture reflects the
/// materialized context back and the test asserts it, including the `--clove-dir`
/// override path). `provider` is omitted (never set empty) when `None`.
pub fn export_env(
    cmd: &mut Command,
    ctx: &Ctx,
    globals: &PluginGlobals,
    command: &str,
    provider: Option<&str>,
) {
    // Use the authoritative resolved `.clove/` dir (honors `--clove-dir`), never
    // `root.join(".clove")` — under the override those disagree (§6.2).
    let sync_dir = ctx.clove_dir.join("sync");
    let config_path = ctx.clove_dir.join("config.toml");

    // The path to the running host binary, for the plugin's `$CLOVE` callback.
    let clove_bin = std::env::current_exe()
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .map(Utf8PathBuf::into_string)
        .unwrap_or_else(|| "clove".to_owned());

    // Identity & contract.
    cmd.env("CLOVE", clove_bin);
    cmd.env("CLOVE_VERSION", env!("CARGO_PKG_VERSION"));
    cmd.env(
        "CLOVE_SCHEMA",
        clove_types::CURRENT_SCHEMA_VERSION.to_string(),
    );
    cmd.env("CLOVE_PLUGIN_API", PLUGIN_API_VERSION.to_string());
    cmd.env("CLOVE_COMMAND", command);
    match provider {
        Some(provider) => cmd.env("CLOVE_PROVIDER", provider),
        // Explicitly clear it so a stray ambient CLOVE_PROVIDER can't leak into a
        // generic plugin (every other CLOVE_* var is unconditionally set, so only
        // this conditional one needs the guard) — §6.2 "absent → omitted".
        None => cmd.env_remove("CLOVE_PROVIDER"),
    };

    // Repository location (all derived once from the host's `discover()`).
    cmd.env("CLOVE_DIR", ctx.clove_dir.as_str());
    cmd.env("CLOVE_ROOT", ctx.root.as_str());
    cmd.env("CLOVE_ISSUES_DIR", ctx.issues_dir.as_str());
    cmd.env("CLOVE_DB_PATH", ctx.db_path.as_str());
    cmd.env("CLOVE_SYNC_DIR", sync_dir.as_str());
    cmd.env("CLOVE_CONFIG_PATH", config_path.as_str());

    // Resolved config & output.
    cmd.env("CLOVE_ID_PREFIX", &ctx.config.id_prefix);
    cmd.env("CLOVE_FORMAT", globals.format.as_str());
    cmd.env("CLOVE_COLOR", color_wire(globals.color));
    cmd.env("CLOVE_QUIET", bool_wire(globals.quiet));
    cmd.env("CLOVE_NO_INDEX", bool_wire(globals.no_index));
    cmd.env("CLOVE_DEEP", bool_wire(globals.deep));
}

/// Invoke a resolved plugin (§6.1).
///
/// The plugin is called cargo-style: `subcommand_name_echo` (e.g. `["sync",
/// "github"]` or `["echo"]`) is pushed as leading args, then `argv_after_name` —
/// so the same binary also works when invoked directly. On Unix the host
/// **replaces its own process image** with [`CommandExt::exec`], which only
/// returns on failure (mapped to an I/O error). On Windows it spawns and waits,
/// propagating the child's exit code via [`std::process::exit`].
pub fn run_plugin(
    path: &Utf8Path,
    argv_after_name: &[String],
    subcommand_name_echo: &[&str],
    ctx: &Ctx,
    globals: &PluginGlobals,
    command: &str,
    provider: Option<&str>,
) -> Result<ExitCode, CloveError> {
    let mut cmd = Command::new(path);
    for segment in subcommand_name_echo {
        cmd.arg(segment);
    }
    cmd.args(argv_after_name);
    export_env(&mut cmd, ctx, globals, command, provider);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // `exec` replaces this image; it returns only if the exec itself failed.
        let err = cmd.exec();
        Err(CloveError::Io {
            path: path.to_owned(),
            source: err,
        })
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().map_err(|source| CloveError::Io {
            path: path.to_owned(),
            source,
        })?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_name_joins_segments_and_prefix() {
        // The suffix is empty on Unix and `.exe` on Windows; assert the stable
        // prefix + join and that the platform suffix is appended.
        assert_eq!(
            binary_name(&["sync", "github"]),
            format!("clove-sync-github{}", std::env::consts::EXE_SUFFIX)
        );
        assert_eq!(
            binary_name(&["echo"]),
            format!("clove-echo{}", std::env::consts::EXE_SUFFIX)
        );
    }

    #[test]
    fn rejects_traversal_segments() {
        assert!(is_valid_segment("sync"));
        assert!(is_valid_segment("sync-github"));
        assert!(!is_valid_segment(""));
        assert!(!is_valid_segment(".."));
        assert!(!is_valid_segment("foo/../bin/sh"));
        assert!(!is_valid_segment("a\\b"));
        // A traversal token never resolves to a path outside a search dir.
        assert_eq!(resolve(&["foo/../../bin/sh"]), None);
    }

    #[test]
    fn wire_spellings_match_the_contract() {
        assert_eq!(color_wire(ColorChoice::Auto), "auto");
        assert_eq!(color_wire(ColorChoice::Always), "always");
        assert_eq!(color_wire(ColorChoice::Never), "never");
        assert_eq!(bool_wire(true), "1");
        assert_eq!(bool_wire(false), "0");
    }

    #[test]
    fn host_plugin_api_is_single_sourced_from_clove_plugin() {
        // The host constant is the re-exported `clove-plugin` one, so the value
        // the host threads into the env and compares against a probe can never
        // drift from what a plugin advertises.
        assert_eq!(PLUGIN_API_VERSION, clove_plugin::CLOVE_PLUGIN_API);
    }

    #[test]
    fn run_as_maps_provides_tokens() {
        assert_eq!(
            run_as(&["sync:github".to_owned()], "sync-github"),
            vec!["clove sync github"]
        );
        assert_eq!(
            run_as(&["import:tk".to_owned()], "import-tk"),
            vec!["clove import tk"]
        );
        // A bare token (no `:`) becomes `clove <token>`.
        assert_eq!(run_as(&["echo".to_owned()], "echo"), vec!["clove echo"]);
        // Multiple tokens → multiple command lines.
        assert_eq!(
            run_as(
                &["sync:gitlab".to_owned(), "import:gitlab".to_owned()],
                "gitlab"
            ),
            vec!["clove sync gitlab", "clove import gitlab"]
        );
    }

    #[test]
    fn run_as_falls_back_to_name_heuristic() {
        // No `provides` → split a mux prefix off the binary name.
        assert_eq!(run_as(&[], "sync-github"), vec!["clove sync github"]);
        assert_eq!(run_as(&[], "import-tk"), vec!["clove import tk"]);
        assert_eq!(run_as(&[], "export-csv"), vec!["clove export csv"]);
        // A non-mux name is a generic subcommand.
        assert_eq!(run_as(&[], "frobnicate"), vec!["clove frobnicate"]);
    }

    #[test]
    fn parse_probe_json_reads_all_fields() {
        let json = r#"{
            "name":"clove-sync-github","version":"0.2.0",
            "about":"Two-way GitHub sync","provides":["sync:github"],
            "clove_plugin_api":1,"min_clove_plugin_api":1,"max_clove_plugin_api":1,
            "max_schema":1
        }"#;
        let probed = parse_probe_json(json).expect("parses");
        assert_eq!(probed.version, "0.2.0");
        assert_eq!(probed.about, "Two-way GitHub sync");
        assert_eq!(probed.provides, vec!["sync:github"]);
        assert_eq!(probed.clove_plugin_api, 1);
        assert_eq!(probed.min_clove_plugin_api, 1);
        assert_eq!(probed.max_clove_plugin_api, 1);
        assert_eq!(PluginStatus::classify(&probed), PluginStatus::Ok);
    }

    #[test]
    fn parse_probe_json_defaults_missing_compat_to_host() {
        // A legacy plugin that answers with only the original keys is treated as
        // declaring the host contract version (all-v1 Phase 1) → ok, not no_info.
        let json = r#"{"name":"clove-echo","version":"0.1.0","about":"x","provides":["echo"]}"#;
        let probed = parse_probe_json(json).expect("parses");
        assert_eq!(probed.min_clove_plugin_api, PLUGIN_API_VERSION);
        assert_eq!(probed.max_clove_plugin_api, PLUGIN_API_VERSION);
        assert_eq!(PluginStatus::classify(&probed), PluginStatus::Ok);
    }

    #[test]
    fn parse_probe_json_rejects_garbage() {
        assert!(parse_probe_json("not json").is_none());
        assert!(parse_probe_json("").is_none());
    }

    #[test]
    fn status_classification_matches_the_range_rule() {
        let probe = |min, max| ProbedInfo {
            version: String::new(),
            about: String::new(),
            provides: vec![],
            clove_plugin_api: min,
            min_clove_plugin_api: min,
            max_clove_plugin_api: max,
            max_schema: 1,
        };
        let host = PLUGIN_API_VERSION;
        // min ≤ host ≤ max → ok.
        assert_eq!(PluginStatus::classify(&probe(host, host)), PluginStatus::Ok);
        // host > max → outdated.
        assert_eq!(
            PluginStatus::classify(&probe(host, host - 1)),
            PluginStatus::Outdated
        );
        // host < min → needs newer clove.
        assert_eq!(
            PluginStatus::classify(&probe(host + 1, host + 1)),
            PluginStatus::NeedsNewerClove
        );
    }

    #[test]
    fn status_wire_spellings() {
        assert_eq!(PluginStatus::Ok.as_str(), "ok");
        assert_eq!(PluginStatus::Outdated.as_str(), "outdated");
        assert_eq!(PluginStatus::NeedsNewerClove.as_str(), "needs_newer_clove");
        assert_eq!(PluginStatus::NoInfo.as_str(), "no_info");
    }
}
