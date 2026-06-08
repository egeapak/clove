//! `clove status`/`start`/`close` (T-CLI06).

use clove_core::OutputFormat;
use clove_types::{CloveError, ItemFrontmatter, ItemStatus};
use serde_json::Map;

use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id};

/// Apply a status transition to frontmatter, maintaining the closed-timestamp
/// invariant (delegates to the shared [`clove_types::set_status`]).
pub fn set_status(fm: &mut ItemFrontmatter, status: ItemStatus) {
    clove_types::set_status(fm, status, now_seconds());
}

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    status: ItemStatus,
    quiet: bool,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    let mut item = ctx.store.get(&id)?;
    set_status(&mut item.frontmatter, status);

    // Closing an item that others depend on is allowed, but warned about.
    if status == ItemStatus::Closed && !quiet {
        let dependents = dependents_of(ctx, &id);
        if !dependents.is_empty() {
            eprintln!(
                "warning: {} still has open dependents: {}",
                id.as_str(),
                dependents.join(", ")
            );
        }
    }

    let saved = ctx.store.update(&item, now_seconds())?;
    print_item(format, &saved, Map::new());
    Ok(())
}

/// IDs of items whose `deps` list references `id` (best-effort).
fn dependents_of(ctx: &Ctx, id: &clove_types::CloveId) -> Vec<String> {
    let (frontmatters, _errors) = ctx.store.scan_frontmatter().unwrap_or_default();
    frontmatters
        .into_iter()
        .filter(|fm| fm.status != ItemStatus::Closed && fm.deps.contains(id))
        .map(|fm| fm.id.to_string())
        .collect()
}
