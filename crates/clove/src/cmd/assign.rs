//! `clove assign <id> [who] [--clear]` (T-CLI07).

use clove_core::OutputFormat;
use clove_types::CloveError;
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
    let mut item = ctx.store.get(&id)?;

    item.frontmatter.assignee = if clear {
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

    let saved = ctx.store.update(&item, now_seconds())?;
    print_item(format, &saved, Map::new());
    Ok(())
}
