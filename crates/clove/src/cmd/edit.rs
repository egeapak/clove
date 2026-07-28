//! `clove edit` (T-CLI05) and the shared non-interactive field application used
//! by `clove set`.

use clove_core::{parse_item_file, ItemStore, OutputFormat};
use clove_types::{CloveError, CloveId};
use serde_json::Map;

use crate::cli::EditArgs;
use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id};

/// Apply `KEY=VALUE` (and `labels+=`/`labels-=`) assignments to `id` atomically,
/// returning the saved item. Shared by `clove edit --field` and `clove set`.
///
/// The read-modify-write runs under one store-wide lock (`update_with`), not a
/// lock-free `get` followed by a locking `update`: the latter leaves a window in
/// which a concurrent writer (web, MCP, daemon) can commit between the read and
/// the write, and have its update silently clobbered (DESIGN §4).
pub fn apply_assignments_to(
    store: &ItemStore,
    id: &CloveId,
    assignments: &[String],
) -> Result<clove_types::Item, CloveError> {
    let now = now_seconds();
    store.update_with(id, now, |item| {
        clove_types::apply_assignments(&mut item.frontmatter, assignments, now)
    })
}

pub fn run(ctx: &Ctx, format: OutputFormat, args: EditArgs) -> Result<(), CloveError> {
    let id = parse_id(&args.id)?;

    if args.fields.is_empty() {
        return open_in_editor(ctx, &id);
    }

    let saved = apply_assignments_to(&ctx.store, &id, &args.fields)?;
    print_item(format, &saved, Map::new());
    Ok(())
}

/// Open the item file in `$EDITOR`/`$VISUAL`, then re-parse to validate it.
fn open_in_editor(ctx: &Ctx, id: &clove_types::CloveId) -> Result<(), CloveError> {
    let path = ctx.store.path_for(id);
    if !path.exists() {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let editor = std::env::var("CLOVE_EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());

    let status = std::process::Command::new(&editor)
        .arg(path.as_str())
        .status()
        .map_err(|source| CloveError::Io {
            path: path.clone(),
            source,
        })?;
    if !status.success() {
        return Err(CloveError::Io {
            path: path.clone(),
            source: std::io::Error::other(format!("editor `{editor}` exited with failure")),
        });
    }

    // Validate that the result still parses (surfaces a corrupt hand-edit).
    parse_item_file(&path)?;
    Ok(())
}
