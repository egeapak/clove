//! clove-plugin: the support crate every first-party `clove-*` subcommand plugin
//! depends on.
//!
//! It provides two things so a plugin author never touches `std::env` or
//! re-implements the output contract:
//!
//! - [`PluginContext`] — the typed materialization of the host↔plugin
//!   environment contract (`PLUGIN_SYSTEM.md` §6.2/§6.5). The host writes those
//!   vars from one place (`plugin::export_env`); this reads them back into the
//!   same shape as the host's own `context::Ctx`.
//! - [`run`] / [`run_with_info`] — the envelope + exit-code harness
//!   (`PLUGIN_SYSTEM.md` §6.3). It calls [`PluginContext::from_env`], parses
//!   argv into [`PluginArgs`], runs the closure, and renders the standard
//!   `{ v, ok, data, _meta }` success / `{ v, ok: false, error }` failure
//!   envelope honoring `cx.format`, returning the correct [`ExitCode`].
//!
//! The envelope writer + `CloveError` classification live in [`envelope`] and
//! reuse [`clove_types::error_code`], so a plugin reports the identical
//! `code`/`exit` a built-in would for the same failure.
#![deny(clippy::print_stdout)] // print via `outln!`/`out!` (see `clove_plugin::stdout`)

#[cfg(feature = "clap")]
mod clap_util;
mod context;
mod envelope;
mod host;
mod run;
pub mod stdout;

#[cfg(feature = "clap")]
pub use clap_util::{clap_exit_code, parse_format};
pub use context::{PluginColor, PluginContext, PluginEnvError};
pub use envelope::{emit_error, emit_success, emit_success_with_meta, ENVELOPE_VERSION};
pub use host::{HostClient, HostError, HostInfo, HOST_API};
pub use run::{
    info_requested, run, run_with_info, unsupported_capability, PluginArgs, PluginInfo,
    CLOVE_PLUGIN_API,
};

// Re-exported so a plugin's closure signature can name them without adding
// clove-types to its own dependency list for the common case. This crate never
// depends on clove-core: it is the permissively licensed (MIT OR Apache-2.0)
// plugin SDK, and clove-core is AGPL. A plugin that opens the store itself
// depends on clove-core directly (and is then a combined work under the AGPL).
/// The host IPC call arguments (the MCP tool arguments), for [`HostClient`].
pub use clove_types::tool;
pub use clove_types::{CloveError, OutputFormat};
