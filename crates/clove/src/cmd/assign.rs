//! `clove assign <id> [who] [--clear]` (T-CLI07).
//!
//! Thin shim over the unified [`EditRequest`] path — `Some(None)` clears,
//! `Some(Some(who))` sets — so assignee validation lives in one place.

use clove_core::{apply_edit, OutputFormat};
use clove_types::{CloveError, EditRequest};
use serde_json::Map;

use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id};

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    assignee: Option<String>,
    clear: bool,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;

    // Argument-shape check stays here (it is about the flag pairing, not the
    // field value): with neither a name nor --clear there is nothing to do.
    let assignee = if clear {
        None
    } else {
        match assignee {
            Some(a) if !a.trim().is_empty() => Some(a),
            _ => {
                return Err(CloveError::InvalidField {
                    field: "assignee".to_owned(),
                    reason: "provide an assignee or use --clear".to_owned(),
                })
            }
        }
    };

    let req = EditRequest {
        assignee: Some(assignee),
        ..EditRequest::default()
    };
    apply_edit(&ctx.store, &id, &req, now_seconds())?;

    let saved = ctx.store.get(&id)?;
    print_item(format, &saved, Map::new());
    Ok(())
}
