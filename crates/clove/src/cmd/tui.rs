//! `clove tui` (M4): launch the read-only terminal browser.
//!
//! Hands the discovered file store to `clove-tui`, which takes over the
//! terminal until the user quits. This command produces no JSON/structured
//! output — it is interactive-only — so it ignores the output format.

use clove_core::{CloveError, ItemStore, OutputFormat};

use crate::context::Ctx;

pub fn run(ctx: &Ctx, _format: OutputFormat) -> Result<(), CloveError> {
    let store = ItemStore::new(ctx.root.clone());
    clove_tui::run(store).map_err(|e| CloveError::Io {
        path: ctx.root.clone(),
        source: std::io::Error::other(e.to_string()),
    })
}
