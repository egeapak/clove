//! `clove status`/`start`/`close` (T-CLI06).

use clove_core::OutputFormat;
use clove_types::{CloveError, ItemStatus};
use serde_json::Map;

use crate::context::Ctx;
use crate::item_json::print_item;
use crate::util::{now_seconds, parse_id};

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    status: ItemStatus,
    quiet: bool,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    let now = now_seconds();

    // The read-modify-write runs under one store-wide lock (`update_with`), not
    // a lock-free `get` followed by a locking `update`: the latter leaves a
    // window in which a concurrent writer (web, MCP, daemon) can commit between
    // the read and the write, and have its update silently clobbered. Extra
    // *reads* inside the closure are covered by the same lock (DESIGN §4).
    let saved = ctx.store.update_with(&id, now, |item| {
        clove_types::set_status(&mut item.frontmatter, status, now);

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
        Ok(())
    })?;
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
