//! `clove mcp` (M4): run the MCP server over stdio so AI agents can drive clove
//! as native tools.
//!
//! This is interactive/transport-only (it owns stdin/stdout for the JSON-RPC
//! framing), so it ignores `--format`. The actual server lives in the `clove-mcp`
//! crate behind the default-on `mcp` feature; a build without that feature still
//! exposes the subcommand but errors cleanly.

use clove_types::CloveError;

use crate::context::Ctx;

#[cfg(feature = "mcp")]
pub fn run(ctx: &Ctx) -> Result<(), CloveError> {
    let clove_dir = ctx
        .issues_dir
        .parent()
        .map(|p| p.to_owned())
        .unwrap_or_else(|| ctx.root.join(".clove"));
    clove_mcp::run(
        clove_dir,
        ctx.root.clone(),
        ctx.config.id_prefix.clone(),
        ctx.config.default_type,
    )
    .map_err(|e| CloveError::Io {
        path: ctx.root.clone(),
        source: std::io::Error::other(e.to_string()),
    })
}

#[cfg(not(feature = "mcp"))]
pub fn run(ctx: &Ctx) -> Result<(), CloveError> {
    Err(CloveError::Io {
        path: ctx.root.clone(),
        source: std::io::Error::other(
            "this clove binary was built without MCP support (enable the `mcp` feature)",
        ),
    })
}
