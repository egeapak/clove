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
//! - the **invocation** ([`run_plugin`]) — spawn-and-wait on every platform
//!   (stdio inherited, so the plugin gets the real tty and unbuffered output),
//!   exiting the way the plugin exited; signal handling lives in
//!   [`crate::plugin_child`] (§6.1).

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
/// suffix stripped) and the absolute path that would be run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInfo {
    /// The subcommand name, e.g. `sync-github` for `clove-sync-github`.
    pub name: String,
    /// The resolved path to the plugin binary.
    pub path: Utf8PathBuf,
}

/// The candidate binary name for a dispatch path, e.g. `["sync", "github"]` →
/// `clove-sync-github` (plus the platform executable suffix, `.exe` on Windows).
#[cfg(test)]
fn binary_name(segments: &[&str]) -> String {
    binary_name_with(segments, std::env::consts::EXE_SUFFIX)
}

/// [`binary_name`] with the executable suffix injected — see
/// [`crate::registry::install::installed_binary_path_with`] for why. The search
/// itself tries every extension in [`exe_extensions`], one at a time.
fn binary_name_with(segments: &[&str], exe_suffix: &str) -> String {
    format!("clove-{}{exe_suffix}", segments.join("-"))
}

/// The ordered plugin search path (§5): the directory of the running `clove`
/// binary, then each dir in `$CLOVE_PLUGIN_PATH`, then the clove-managed
/// `<clove-home>/bin`, then each dir on `$PATH`.
///
/// The current-exe directory comes first so a plugin installed next to `clove`
/// (the common `cargo install` case) is found even when that dir is not on
/// `$PATH`. Splitting uses the platform path separator (`;` on Windows else `:`).
///
/// `<clove-home>/bin` — where `clove plugin install` puts binaries — sits
/// **after** `$CLOVE_PLUGIN_PATH` and **before** `$PATH`. The order is
/// deliberate: `$CLOVE_PLUGIN_PATH` is the user's explicit opt-in directory, so
/// a binary clove fetched from the internet must never outrank a deliberate
/// local override; but a clove-managed install should still win over an
/// incidental `$PATH` entry.
fn search_dirs() -> Vec<Utf8PathBuf> {
    let mut dirs: Vec<Utf8PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if let Ok(dir) = Utf8PathBuf::from_path_buf(parent.to_owned()) {
                dirs.push(dir);
            }
        }
    }

    push_env_dirs(&mut dirs, "CLOVE_PLUGIN_PATH");
    // An unresolvable clove home (no `$HOME`) simply contributes no entry.
    if let Some(bin) = crate::clove_home::bin_dir() {
        dirs.push(bin);
    }
    push_env_dirs(&mut dirs, "PATH");

    dirs
}

/// Append every directory listed in the path-style environment variable `var`.
fn push_env_dirs(dirs: &mut Vec<Utf8PathBuf>, var: &str) {
    if let Some(value) = std::env::var_os(var) {
        for path in std::env::split_paths(&value) {
            if let Ok(dir) = Utf8PathBuf::from_path_buf(path) {
                dirs.push(dir);
            }
        }
    }
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
        // Windows has no execute bit: a file is runnable because of its
        // extension. Every caller only builds or accepts names that carry an
        // extension from [`exe_extensions`] (`PATHEXT`), so a plain file is
        // enough here.
        true
    }
}

/// The `PATHEXT` used when the variable is unset or lists nothing usable. This
/// is the Windows default minus the legacy script hosts (`.VBS`, `.JS`, …),
/// which clove would have no way to run.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The executable extensions to try, in precedence order, for one plugin stem.
///
/// On Unix this is only the empty suffix: a plugin is `clove-<x>` with an
/// execute bit. On Windows it is `$PATHEXT` (§5), so a plugin shipped as
/// `clove-<x>.cmd`, `.bat`, or (when the user lists it) `.ps1` is found, not
/// only `clove-<x>.exe`.
fn exe_extensions() -> Vec<String> {
    if cfg!(windows) {
        extensions_from_pathext(std::env::var("PATHEXT").ok().as_deref())
    } else {
        vec![String::new()]
    }
}

/// Parse a `PATHEXT` value into lowercase extensions, in order and deduplicated.
///
/// An entry that does not start with `.` is ignored, as Windows itself does. An
/// unset or empty value falls back to [`DEFAULT_PATHEXT`]. `.exe` is always
/// present (appended when the value leaves it out), because every plugin that
/// `cargo install` writes is an `.exe`, and an unusual `PATHEXT` must not hide
/// the shipped plugins.
fn extensions_from_pathext(value: Option<&str>) -> Vec<String> {
    let parse = |v: &str| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for entry in v.split(';') {
            let ext = entry.trim().to_ascii_lowercase();
            if ext.len() > 1 && ext.starts_with('.') && !out.contains(&ext) {
                out.push(ext);
            }
        }
        out
    };
    let mut exts = value.map(parse).unwrap_or_default();
    if exts.is_empty() {
        exts = parse(DEFAULT_PATHEXT);
    }
    if !exts.iter().any(|e| e == ".exe") {
        exts.push(".exe".to_owned());
    }
    exts
}

/// The extension of `file_name` that is in `exts`, compared without regard to
/// case (Windows file names are case-insensitive), as an index into `exts`.
///
/// The empty extension (Unix) matches every name. Returns the stem (the name
/// without the extension) with the index.
fn split_exe_extension<'a>(file_name: &'a str, exts: &[String]) -> Option<(&'a str, usize)> {
    exts.iter().enumerate().find_map(|(rank, ext)| {
        if ext.is_empty() {
            return Some((file_name, rank));
        }
        let cut = file_name.len().checked_sub(ext.len())?;
        let (stem, tail) = (file_name.get(..cut)?, file_name.get(cut..)?);
        tail.eq_ignore_ascii_case(ext).then_some((stem, rank))
    })
}

/// The `Command` that runs the plugin at `path`.
///
/// `.exe`/`.com` files, and every Unix plugin, run directly. Rust's standard
/// library runs a `.bat`/`.cmd` file through `cmd.exe` itself, with argument
/// escaping. A `.ps1` script cannot be started by `CreateProcess`, so it runs
/// as `powershell.exe -NoLogo -NoProfile -File <path> …`. The user's execution
/// policy still applies; clove does not bypass it.
fn plugin_command(path: &Utf8Path) -> Command {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ps1"))
    {
        let mut cmd = Command::new("powershell.exe");
        cmd.args(["-NoLogo", "-NoProfile", "-File"]).arg(path);
        cmd
    } else {
        Command::new(path)
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
    let exts = exe_extensions();
    search_dirs()
        .iter()
        .find_map(|dir| resolve_in(dir, segments, &exts))
}

/// The first executable `clove-<segments>` in `dir`, trying `exts` in order.
///
/// Directory comes before extension, as in a Windows command lookup: a
/// `clove-foo.cmd` early on the search path wins over a `clove-foo.exe` later on
/// it, and within one directory `PATHEXT` order decides.
fn resolve_in(dir: &Utf8Path, segments: &[&str], exts: &[String]) -> Option<Utf8PathBuf> {
    exts.iter()
        .map(|ext| dir.join(binary_name_with(segments, ext)))
        .find(|candidate| is_executable(candidate))
}

/// The ordered umbrella candidate *home multiplexers* to try for a `<mux>
/// <provider>` request (`PLUGIN_SYSTEM.md` §4.2). The dedicated home mux is always
/// first, so an explicitly-installed `clove-<mux>-<provider>` wins; then the
/// bidirectional `sync` umbrella (a sync plugin serves import/export from one
/// binary); then the cross-sibling one-way binary, which may itself be
/// multi-capability (e.g. `clove-import-beads` also providing `export:beads`).
/// `sync` has no fallback — a one-way binary cannot serve a two-way reconcile.
fn mux_candidates(mux: &str) -> &'static [&'static str] {
    match mux {
        "import" => &["import", "sync", "export"],
        "export" => &["export", "sync", "import"],
        "sync" => &["sync"],
        _ => &[],
    }
}

/// Resolve the plugin binary for `clove <mux> <provider>` (`PLUGIN_SYSTEM.md`
/// §4.2), walking the umbrella candidates ([`mux_candidates`]) and returning the
/// first existing executable. Purely structural — no `--clove-plugin-info` probe,
/// no exec — so it stays on the same cheap `stat`-only hot path as [`resolve`].
///
/// The candidate order is load-bearing: because dispatch runs the first
/// *existing* binary (it does not fall through to a later candidate once one is
/// found), the order must put the most-correct binary first. A binary reached for
/// a capability it does not implement is expected to reject the request itself
/// (`clove_plugin::unsupported_capability`, exit 2).
pub fn resolve_mux(mux: &str, provider: &str) -> Option<Utf8PathBuf> {
    mux_candidates(mux)
        .iter()
        .find_map(|home| resolve(&[home, provider]))
}

/// Enumerate every resolvable `clove-<x>` plugin along the §5 search path.
///
/// Files are matched on the `clove-<x>` name shape and executability, deduped by
/// name (first match wins, mirroring [`resolve`]'s precedence), and returned
/// sorted by name. The host's own adjacent binaries (`clove`, `cloved`) never
/// carry the `clove-` prefix and so are excluded automatically.
pub fn list() -> Vec<PluginInfo> {
    list_in(&search_dirs(), &exe_extensions())
}

/// [`list`] over explicit search dirs and extensions, so the extension rules are
/// testable on every platform.
fn list_in(dirs: &[Utf8PathBuf], exts: &[String]) -> Vec<PluginInfo> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut plugins: Vec<PluginInfo> = Vec::new();

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        // Within one directory the best-ranked extension wins (`PATHEXT`
        // order, as [`resolve_in`] tries them), whatever order `read_dir`
        // returns the files in. Keyed by name → (rank, path).
        let mut best: std::collections::BTreeMap<String, (usize, Utf8PathBuf)> =
            std::collections::BTreeMap::new();
        for entry in entries.flatten() {
            let Ok(file_name) = entry.file_name().into_string() else {
                continue;
            };
            // The host's own binaries are never plugins (defensive; they lack the
            // `clove-` prefix anyway).
            if file_name == "clove" || file_name == "cloved" {
                continue;
            }
            let Some((stem, rank)) = split_exe_extension(&file_name, exts) else {
                continue;
            };
            // The same rule `provenance::bare_subcommand` applies, from the same
            // function — `require_suffix` is the one deliberate difference, and
            // it is documented there. The extension is already stripped above,
            // so no suffix is passed here.
            let Some(name) = crate::registry::provenance::subcommand_of_file(stem, "", true) else {
                continue;
            };
            let path = dir.join(&file_name);
            if !is_executable(&path) {
                continue;
            }
            match best.get(name) {
                Some((held, _)) if *held <= rank => {}
                _ => {
                    best.insert(name.to_owned(), (rank, path));
                }
            }
        }
        for (name, (_, path)) in best {
            if seen.insert(name.clone()) {
                plugins.push(PluginInfo { name, path });
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
    ///
    /// Phase 1 gates only on the `clove_plugin_api` range. `probed.max_schema`
    /// (the highest on-disk item schema the plugin can read) is parsed and carried
    /// for the registry/`plugin list` surface but is intentionally *not* a compat
    /// gate yet — schema-version gating is deferred to the registry phase, where
    /// install-time verification owns it. Wiring it here would change dispatch
    /// behavior in the all-v1 Phase 1 with no plugin able to exercise it.
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

/// [`PROBE_TIMEOUT`], or `CLOVE_PLUGIN_PROBE_MS` in a `test-knobs` build.
fn probe_timeout() -> std::time::Duration {
    crate::knobs::duration("CLOVE_PLUGIN_PROBE_MS").unwrap_or(PROBE_TIMEOUT)
}

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

    let mut child = plugin_command(path)
        .arg("--clove-plugin-info")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let start = std::time::Instant::now();
    let timeout = probe_timeout();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= timeout {
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
    // Bounded. The probe timeout covers only `try_wait`, so once the direct
    // child exits this read had no cap and no deadline: a plugin that forks a
    // grandchild inheriting the stdout pipe and then exits leaves the write end
    // open forever, and this blocked with no timeout left to save it. `take` is
    // enough to bound the damage — a metadata reply is a few hundred bytes, and
    // this now runs against freshly-downloaded third-party code (gate 3).
    const MAX_PROBE_OUTPUT: u64 = 64 * 1024;
    child
        .stdout
        .take()?
        .take(MAX_PROBE_OUTPUT)
        .read_to_string(&mut stdout)
        .ok()?;

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
    // The probe payload must be a JSON *object*. A bare scalar/array is valid JSON
    // but every `value[key]` below would silently resolve to `Null` and default,
    // listing a misbehaving plugin as a healthy one with blank metadata. Reject it
    // so it collapses to `no_info` like any other unparseable output.
    if !value.is_object() {
        return None;
    }

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
/// so the same binary also works when invoked directly. The host spawns it with
/// inherited stdio, waits, and exits with the plugin's exit status (a plugin
/// killed by a signal kills the host with the same signal); see
/// [`crate::plugin_child`]. It returns only if the spawn itself failed (mapped to
/// an I/O error).
pub fn run_plugin(
    path: &Utf8Path,
    argv_after_name: &[String],
    subcommand_name_echo: &[&str],
    ctx: &Ctx,
    globals: &PluginGlobals,
    command: &str,
    provider: Option<&str>,
) -> Result<ExitCode, CloveError> {
    let mut cmd = plugin_command(path);
    for segment in subcommand_name_echo {
        cmd.arg(segment);
    }
    cmd.args(argv_after_name);
    export_env(&mut cmd, ctx, globals, command, provider);
    // The host IPC channel (§6.6). A failure to bind is not fatal: the plugin
    // runs without it and its SDK reports `HostError::Unavailable` if it asks.
    let host = match crate::plugin_host::HostServer::start(ctx, globals) {
        Ok(host) => {
            host.export(&mut cmd);
            Some(host)
        }
        Err(e) => {
            crate::plugin_host::clear_env(&mut cmd);
            if !globals.quiet {
                eprintln!("warning: plugin host channel unavailable: {e}");
            }
            None
        }
    };
    Err(crate::plugin_child::run_and_exit(&mut cmd, path, || {
        if let Some(host) = &host {
            host.cleanup();
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suffix_aware_helpers_agree_under_an_injected_suffix() {
        use crate::registry::{install, provenance};

        // Two Windows-only bugs shipped on this branch — a path built without
        // `EXE_SUFFIX`, and a suffixed name reaching `cargo --bin` — and CI runs
        // the suite on Windows. It could not catch them: the install suite is
        // `#![cfg(unix)]`, and the existing suffix assertions are written as
        // `format!("…{}", EXE_SUFFIX)`, which on Unix expands to the empty
        // string and asserts nothing about suffix handling at all.
        //
        // So the round trip is pinned under both suffixes, on every platform.
        for suffix in ["", ".exe"] {
            let file = binary_name_with(&["sync", "github"], suffix);
            assert_eq!(file, format!("clove-sync-github{suffix}"));

            let root = camino::Utf8Path::new("/r");
            let path = install::installed_binary_path_with(root, "clove-sync-github", suffix);
            assert_eq!(
                path,
                root.join("bin").join(&file),
                "the probed path must be the file cargo actually wrote, or gate 3 \
                 stats a file that is not there, finds nothing, and passes vacuously"
            );

            assert_eq!(
                provenance::bare_subcommand_with(path.file_name().unwrap(), suffix),
                Some("sync-github")
            );
            // cargo's bookkeeping is the only place the suffixed name appears,
            // so an unsuffixed entry must still resolve.
            assert_eq!(
                provenance::bare_subcommand_with("clove-sync-github", suffix),
                Some("sync-github")
            );
        }
    }

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

    fn exts(list: &[&str]) -> Vec<String> {
        list.iter().map(|e| (*e).to_owned()).collect()
    }

    /// Write an executable file (the execute bit matters on Unix only).
    fn touch_exec(path: &Utf8Path) {
        std::fs::write(path, b"").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn pathext_is_parsed_in_order_lowercased_and_deduped() {
        assert_eq!(
            extensions_from_pathext(Some(".COM;.EXE;.BAT;.CMD;.VBS;.PS1")),
            exts(&[".com", ".exe", ".bat", ".cmd", ".vbs", ".ps1"])
        );
        // Blank entries, entries without a dot, and repeats are dropped.
        assert_eq!(
            extensions_from_pathext(Some(" .Cmd ;;cmd;.;.CMD;.exe")),
            exts(&[".cmd", ".exe"])
        );
    }

    #[test]
    fn pathext_falls_back_and_always_keeps_exe() {
        let default = exts(&[".com", ".exe", ".bat", ".cmd"]);
        assert_eq!(extensions_from_pathext(None), default);
        assert_eq!(extensions_from_pathext(Some("")), default);
        assert_eq!(extensions_from_pathext(Some(";nodot;")), default);
        // A PATHEXT without `.exe` still finds the cargo-installed plugins.
        assert_eq!(
            extensions_from_pathext(Some(".CMD;.PS1")),
            exts(&[".cmd", ".ps1", ".exe"])
        );
    }

    #[test]
    fn exe_extensions_match_the_platform() {
        let found = exe_extensions();
        if cfg!(windows) {
            assert!(found.iter().any(|e| e == ".exe"));
        } else {
            assert_eq!(found, exts(&[""]));
        }
    }

    #[test]
    fn split_exe_extension_ignores_case_and_reports_rank() {
        let win = exts(&[".exe", ".cmd", ".ps1"]);
        assert_eq!(
            split_exe_extension("clove-foo.exe", &win),
            Some(("clove-foo", 0))
        );
        assert_eq!(
            split_exe_extension("clove-foo.CMD", &win),
            Some(("clove-foo", 1))
        );
        assert_eq!(
            split_exe_extension("clove-foo.Ps1", &win),
            Some(("clove-foo", 2))
        );
        assert_eq!(split_exe_extension("clove-foo", &win), None);
        assert_eq!(split_exe_extension("clove-foo.txt", &win), None);
        // A multi-byte name never panics on the cut.
        assert_eq!(split_exe_extension("é", &win), None);
        // Unix: the empty extension matches the whole name.
        assert_eq!(
            split_exe_extension("clove-foo", &exts(&[""])),
            Some(("clove-foo", 0))
        );
    }

    #[test]
    fn resolve_in_finds_script_plugins_in_pathext_order() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8Path::from_path(tmp.path()).unwrap();
        let win = exts(&[".exe", ".bat", ".cmd", ".ps1"]);

        touch_exec(&dir.join("clove-script.cmd"));
        assert_eq!(
            resolve_in(dir, &["script"], &win),
            Some(dir.join("clove-script.cmd"))
        );
        touch_exec(&dir.join("clove-ps.ps1"));
        assert_eq!(
            resolve_in(dir, &["ps"], &win),
            Some(dir.join("clove-ps.ps1"))
        );
        // `.exe` ranks before `.cmd`, so it wins when both exist.
        touch_exec(&dir.join("clove-script.exe"));
        assert_eq!(
            resolve_in(dir, &["script"], &win),
            Some(dir.join("clove-script.exe"))
        );
        // An extension outside the list is not a plugin.
        touch_exec(&dir.join("clove-text.txt"));
        assert_eq!(resolve_in(dir, &["text"], &win), None);
    }

    #[test]
    fn list_in_enumerates_script_plugins_and_prefers_the_best_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let first = Utf8Path::from_path(tmp.path()).unwrap().join("a");
        let second = Utf8Path::from_path(tmp.path()).unwrap().join("b");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let win = exts(&[".exe", ".bat", ".cmd", ".ps1"]);

        touch_exec(&first.join("clove-both.cmd"));
        touch_exec(&first.join("clove-both.exe"));
        touch_exec(&first.join("clove-bat.BAT"));
        touch_exec(&first.join("clove-notes.txt"));
        touch_exec(&first.join("clove-bare"));
        // A later directory never outranks an earlier one, whatever the extension.
        touch_exec(&second.join("clove-bat.exe"));
        touch_exec(&second.join("clove-ps.ps1"));

        let found = list_in(&[first.clone(), second.clone()], &win);
        let pairs: Vec<(&str, &Utf8Path)> = found
            .iter()
            .map(|p| (p.name.as_str(), p.path.as_path()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("bat", first.join("clove-bat.BAT").as_path()),
                ("both", first.join("clove-both.exe").as_path()),
                ("ps", second.join("clove-ps.ps1").as_path()),
            ]
        );
    }

    #[test]
    fn ps1_plugins_run_through_powershell() {
        let cmd = plugin_command(Utf8Path::new("C:/bin/clove-foo.PS1"));
        assert_eq!(cmd.get_program(), "powershell.exe");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(
            args,
            ["-NoLogo", "-NoProfile", "-File", "C:/bin/clove-foo.PS1"]
        );

        let direct = plugin_command(Utf8Path::new("/bin/clove-foo"));
        assert_eq!(direct.get_program(), "/bin/clove-foo");
        assert_eq!(direct.get_args().count(), 0);
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
    fn mux_candidates_order_puts_dedicated_first_then_sync() {
        // Dedicated home mux first (so an installed clove-<mux>-<p> wins), then the
        // bidirectional sync umbrella, then the cross-sibling. `sync` never falls back.
        assert_eq!(mux_candidates("import"), &["import", "sync", "export"]);
        assert_eq!(mux_candidates("export"), &["export", "sync", "import"]);
        assert_eq!(mux_candidates("sync"), &["sync"]);
        assert_eq!(mux_candidates("bogus"), &[] as &[&str]);
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
    fn parse_probe_json_rejects_non_object() {
        // Valid JSON that isn't an object must be rejected — otherwise every
        // `value[key]` resolves to `Null`/default and a misbehaving plugin would
        // list as a healthy one with blank metadata instead of `no_info`.
        assert!(parse_probe_json("42").is_none());
        assert!(parse_probe_json("\"hi\"").is_none());
        assert!(parse_probe_json("[1, 2, 3]").is_none());
        assert!(parse_probe_json("null").is_none());
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
