//! `clove mcp` (M4): run the MCP server over stdio so AI agents can drive clove
//! as native tools.
//!
//! This is interactive/transport-only (it owns stdin/stdout for the JSON-RPC
//! framing), so it ignores `--format`. The actual server lives in the `clove-mcp`
//! crate behind the default-on `mcp` feature; a build without that feature still
//! exposes the subcommand but errors cleanly.
//!
//! Unlike every other repository command, the MCP server **starts even when no
//! `.clove/` exists yet**: a Claude Code plugin spawns `clove mcp` per session,
//! and the server must come up so its tools can return a friendly "no clove
//! repository" error until the user runs `clove init` — rather than the process
//! failing to launch and the client seeing a dead server. So this resolves the
//! repo context itself (with a no-repo fallback) instead of going through the
//! `discover()`-first dispatch path in `main.rs`.

use camino::Utf8Path;
use clove_types::CloveError;

#[cfg(feature = "mcp")]
use crate::context::{current_dir, discover};

#[cfg(feature = "mcp")]
pub fn run(clove_dir_override: Option<&Utf8Path>) -> Result<(), CloveError> {
    use clove_core::CloveConfig;

    // Resolve the repo root, `.clove/` dir, and config. When the repo is
    // discoverable we use its real config (id prefix + default type); when it is
    // absent we fall back to the cwd (or the override's parent) with defaults so
    // the server still starts. Errors other than "no repo" (e.g. a corrupt
    // config) still surface at launch, since they are genuine problems.
    let (repo_root, clove_dir, config) = match discover(clove_dir_override) {
        Ok(ctx) => {
            let clove_dir = ctx
                .issues_dir
                .parent()
                .map(Utf8Path::to_owned)
                .unwrap_or_else(|| ctx.root.join(".clove"));
            (ctx.root, clove_dir, ctx.config)
        }
        // `NoRepo` only arises on the auto-discover (no `--clove-dir`) path:
        // `discover` with an explicit `--clove-dir` roots at the override and
        // derives default config rather than returning `NoRepo`. So the fallback
        // roots at the cwd with defaults; the server starts and its tools report
        // "no clove repository" until `clove init` runs.
        Err(CloveError::NoRepo { .. }) => {
            let root = current_dir()?;
            let clove_dir = root.join(".clove");
            (root, clove_dir, CloveConfig::default())
        }
        Err(e) => return Err(e),
    };

    let err_path = repo_root.clone();
    clove_mcp::run(clove_dir, repo_root, config.id_prefix, config.default_type).map_err(|e| {
        CloveError::Io {
            path: err_path,
            source: std::io::Error::other(e.to_string()),
        }
    })
}

#[cfg(not(feature = "mcp"))]
pub fn run(_clove_dir_override: Option<&Utf8Path>) -> Result<(), CloveError> {
    Err(CloveError::Io {
        path: camino::Utf8PathBuf::from("."),
        source: std::io::Error::other(
            "this clove binary was built without MCP support (enable the `mcp` feature)",
        ),
    })
}
