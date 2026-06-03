//! `clove priority <id> <n>` (T-CLI07).

use clove_core::{CloveError, OutputFormat};
use serde_json::Map;

use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id, parse_priority};

pub fn run(ctx: &Ctx, format: OutputFormat, id: &str, priority: u8) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    let mut item = ctx.store.get(&id)?;
    item.frontmatter.priority = parse_priority(priority)?;
    let saved = ctx.store.update(&item, now_seconds())?;
    print_item(format, &saved, Map::new());
    Ok(())
}
