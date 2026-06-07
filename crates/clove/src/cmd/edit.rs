//! `clove edit` (T-CLI05) and the shared non-interactive field application used
//! by `clove set`.

use clove_core::{parse_item_file, CloveError, ItemFrontmatter, OutputFormat};
use serde_json::Map;

use crate::cli::EditArgs;
use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id};

/// Apply a list of `KEY=VALUE` (and `labels+=`/`labels-=`) edits to frontmatter
/// (delegates to the shared [`clove_core::ops::apply_assignments`]).
pub fn apply_assignments(
    fm: &mut ItemFrontmatter,
    assignments: &[String],
) -> Result<(), CloveError> {
    clove_core::ops::apply_assignments(fm, assignments, now_seconds())
}

pub fn run(ctx: &Ctx, format: OutputFormat, args: EditArgs) -> Result<(), CloveError> {
    let id = parse_id(&args.id)?;

    if args.fields.is_empty() {
        return open_in_editor(ctx, &id);
    }

    let mut item = ctx.store.get(&id)?;
    apply_assignments(&mut item.frontmatter, &args.fields)?;
    let saved = ctx.store.update(&item, now_seconds())?;
    print_item(format, &saved, Map::new());
    Ok(())
}

/// Open the item file in `$EDITOR`/`$VISUAL`, then re-parse to validate it.
fn open_in_editor(ctx: &Ctx, id: &clove_core::CloveId) -> Result<(), CloveError> {
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
