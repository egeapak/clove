//! Index-accelerated read path (T-S06, DESIGN §6.4).
//!
//! When a fresh `index.db` is present, `ls`/`ready`/`query` resolve their
//! filtered, ordered result **directly from SQLite** as a lean projection (the
//! columns the list renders) — no per-item file read. A stale index is
//! transparently refreshed (≤ the threshold) or bypassed. Staleness uses the
//! fast O(readdir) check by default; `--deep` forces the thorough per-file pass.

use clove_core::CloveError;
use clove_index::{Filter, Index, ItemListRow, QueryMode};

use crate::cmd::listing::Filters;
use crate::context::{index_error, Ctx};

/// Above this many out-of-date items, skip the incremental refresh and fall back
/// to a file scan (DESIGN §6.4).
const STALE_REFRESH_LIMIT: usize = 20;

/// The index read result: the lean rows plus any warnings to surface.
pub type IndexList = (Vec<ItemListRow>, Vec<String>);

/// Try to satisfy a list/ready query from the index.
///
/// Returns `Some((rows, warnings))` when the index was used (caller sets
/// `_meta.source = "index"`), or `None` to fall back to the file path. `None` is
/// returned for `--no-index`, a missing/broken index, or one too stale to refresh
/// cheaply.
pub fn list_via_index(
    ctx: &Ctx,
    no_index: bool,
    deep: bool,
    mode: QueryMode,
    filters: &Filters,
) -> Result<Option<IndexList>, CloveError> {
    if no_index || !ctx.db_path.exists() {
        return Ok(None);
    }

    let mut index = match Index::open_or_create(&ctx.db_path) {
        Ok(index) => index,
        // A broken index is non-fatal: fall back to files.
        Err(_) => return Ok(None),
    };

    // Freshen the index unless the repo disables auto-refresh.
    if ctx.config.index.auto_refresh {
        let report = if deep {
            index.check_staleness(&ctx.issues_dir)
        } else {
            index.check_staleness_fast(&ctx.issues_dir)
        }
        .map_err(|e| index_error(e, &ctx.db_path))?;

        if report.change_count() > STALE_REFRESH_LIMIT {
            // Too far behind to refresh inline — use the files instead.
            return Ok(None);
        }
        if !report.is_clean() {
            index
                .apply_staleness(&report, &ctx.issues_dir)
                .map_err(|e| index_error(e, &ctx.db_path))?;
        }
    }

    let filter = Filter {
        mode,
        status: filters.status.map(|s| vec![s]),
        item_type: filters.item_type,
        priority: filters.priority,
        assignee: filters.assignee.clone(),
        label: filters.label.clone(),
        parent: None,
        limit: None,
    };
    let rows = index
        .query_list(&filter)
        .map_err(|e| index_error(e, &ctx.db_path))?;
    Ok(Some((rows, Vec::new())))
}
