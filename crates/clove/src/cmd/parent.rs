//! `clove parent <id> [parent-id] [--clear]`.
//!
//! The parent is a graph edge, so it goes through the cycle-validated
//! [`clove_core::ops::set_parent`] shared with web/MCP/daemon rather than
//! `edit --field`.

use clove_core::OutputFormat;
use clove_types::CloveError;
use serde_json::Map;

use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_millis, parse_id};

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    parent: Option<String>,
    clear: bool,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    let parent = match (parent, clear) {
        (Some(_), true) => {
            return Err(CloveError::InvalidField {
                field: "parent".to_owned(),
                reason: "give a parent id or --clear, not both".to_owned(),
            })
        }
        (None, true) => None,
        (Some(parent), false) => Some(parse_id(&parent)?),
        (None, false) => {
            return Err(CloveError::InvalidField {
                field: "parent".to_owned(),
                reason: "provide a parent id or use --clear".to_owned(),
            })
        }
    };

    clove_core::ops::set_parent(&ctx.store, &id, parent.as_ref(), now_millis())?;

    let saved = ctx.store.get(&id)?;
    print_item(format, &saved, Map::new());
    Ok(())
}
