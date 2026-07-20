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

mod context;
mod envelope;
mod run;

pub use context::{PluginColor, PluginContext, PluginEnvError};
pub use envelope::{emit_error, emit_success, ENVELOPE_VERSION};
pub use run::{run, run_with_info, PluginArgs, PluginInfo};

// Re-exported so a plugin's closure signature can name them without adding
// clove-core/clove-types to its own dependency list for the common case.
pub use clove_core::{CloveConfig, ItemStore, OutputFormat};
pub use clove_types::CloveError;
