//! clove-service: the clove tool surface, independent of any transport.
//!
//! [`Engine`] has one blocking method per tool. `clove-mcp` serves it over MCP
//! stdio; the `clove` host serves it to plugins over the host IPC channel
//! (`docs/PLUGIN_SYSTEM.md` §6.6). The argument types are
//! [`clove_types::tool`], shared with the permissively licensed plugin SDK, so
//! both transports accept and return the same shapes.

mod daemon;
mod engine;
mod error;
pub mod shape;
mod store;

pub use engine::Engine;
pub use error::ToolError;
pub use store::StoreService;
