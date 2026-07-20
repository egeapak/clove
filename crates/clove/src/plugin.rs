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

/// The host↔plugin contract version (`$CLOVE_PLUGIN_API`, §6.2). Bumped only on a
/// breaking change to the env/argv/envelope contract; starts at `1`.
const PLUGIN_API_VERSION: u32 = 1;

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
}
