//! clove MCP server (M4) — feasibility scaffold.
//!
//! This module currently only proves the `rmcp` + `tokio` dependency tree
//! compiles on the workspace toolchain; the real stdio server, tool surface, and
//! daemon (tarpc) routing land in the following phases.

/// Placeholder entry point: returns the MCP protocol/server identity so the
/// dependency on `rmcp` is exercised at compile time.
pub fn server_name() -> &'static str {
    "clove"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rmcp_and_tokio_link() {
        // Touch an rmcp type so the crate is actually linked, and confirm the
        // tokio runtime macro works in this crate.
        let _ = rmcp::model::ProtocolVersion::default();
        assert_eq!(server_name(), "clove");
    }
}
