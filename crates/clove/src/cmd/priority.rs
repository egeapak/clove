//! `clove priority <id> <n>` (T-CLI07).
//!
//! Thin shim over the unified [`EditRequest`] path, like `set`/`edit`/`label`.

use clove_core::{apply_edit, OutputFormat};
use clove_types::{CloveError, EditRequest};
use serde_json::Map;

use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id, parse_priority};

pub fn run(ctx: &Ctx, format: OutputFormat, id: &str, priority: u8) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    let req = EditRequest {
        priority: Some(parse_priority(priority)?),
        ..EditRequest::default()
    };
    apply_edit(&ctx.store, &id, &req, now_seconds())?;
    let saved = ctx.store.get(&id)?;
    print_item(format, &saved, Map::new());
    Ok(())
}
