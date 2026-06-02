//! `clove set <id> KEY=VALUE...` (T-CLI05): alias for `edit --field`.

use clove_core::{CloveError, OutputFormat};
use serde_json::Map;

use crate::cli::SetArgs;
use crate::cmd::edit::apply_assignments;
use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id};

pub fn run(ctx: &Ctx, format: OutputFormat, args: SetArgs) -> Result<(), CloveError> {
    let id = parse_id(&args.id)?;
    let mut item = ctx.store.get(&id)?;
    apply_assignments(&mut item.frontmatter, &args.assignments)?;
    let saved = ctx.store.update(&item, now_seconds())?;
    print_item(format, &saved, Map::new());
    Ok(())
}
