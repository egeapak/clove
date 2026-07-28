//! `clove set <id> KEY=VALUE...` (T-CLI05): alias for `edit --field`.

use clove_core::OutputFormat;
use clove_types::CloveError;
use serde_json::Map;

use crate::cli::SetArgs;
use crate::cmd::edit::apply_assignments_to;
use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::parse_id;

pub fn run(ctx: &Ctx, format: OutputFormat, args: SetArgs) -> Result<(), CloveError> {
    let id = parse_id(&args.id)?;
    let saved = apply_assignments_to(&ctx.store, &id, &args.assignments)?;
    print_item(format, &saved, Map::new());
    Ok(())
}
