//! `clove tui` (M4): launch the terminal browser + add/edit form.
//!
//! Hands the discovered file store (plus the id prefix + default type used when
//! creating items) to `clove-tui`, which takes over the terminal until the user
//! quits. This command produces no JSON/structured output — it is
//! interactive-only — so it ignores the output format.

use clove_core::{ItemStore, OutputFormat};
use clove_types::CloveError;

use crate::context::Ctx;

pub fn run(ctx: &Ctx, _format: OutputFormat) -> Result<(), CloveError> {
    let store = ItemStore::new(ctx.root.clone());
    clove_tui::run(store, ctx.config.id_prefix.clone(), ctx.config.default_type).map_err(|e| {
        CloveError::Io {
            path: ctx.root.clone(),
            source: std::io::Error::other(e.to_string()),
        }
    })
}
