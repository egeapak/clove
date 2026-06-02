//! Index-accelerated read path (T-S06, DESIGN §6.4).
//!
//! When a fresh `index.db` is present, `ls`/`ready`/`query` resolve their
//! filtered, ordered id set from SQLite instead of scanning + graph-building
//! every file. Output objects are still read from the files (the source of
//! truth), so the JSON is byte-for-byte identical to the file-scan path. A stale
//! index is transparently refreshed (≤ the threshold) or bypassed.

use clove_core::{parse_frontmatter_file, CloveError, CloveId, ItemFrontmatter};
use clove_index::{Filter, Index, QueryMode};

use crate::cmd::listing::Filters;
use crate::context::{index_error, Ctx};

/// Above this many out-of-date items, skip the incremental refresh and fall back
/// to a file scan (DESIGN §6.4).
const STALE_REFRESH_LIMIT: usize = 20;

/// The index read result: the ordered, filtered frontmatters plus any warnings.
pub type IndexList = (Vec<ItemFrontmatter>, Vec<String>);

/// Try to satisfy a list/ready query from the index.
///
/// Returns `Some((ordered_frontmatters, warnings))` when the index was used
/// (caller sets `_meta.source = "index"`), or `None` to fall back to the file
/// path. `None` is returned for `--no-index`, a missing/broken index, or one too
/// stale to refresh cheaply.
pub fn list_via_index(
    ctx: &Ctx,
    no_index: bool,
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
        let report = index
            .check_staleness(&ctx.issues_dir)
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
        .query_items(&filter)
        .map_err(|e| index_error(e, &ctx.db_path))?;

    // Read each matched item's frontmatter from its file so output matches the
    // file-scan path exactly. Files that vanished since the refresh are skipped.
    let mut ordered = Vec::with_capacity(rows.len());
    for row in &rows {
        let Ok(id) = CloveId::new(&row.id) else {
            continue;
        };
        if let Ok(fm) = parse_frontmatter_file(&ctx.store.path_for(&id)) {
            ordered.push(fm);
        }
    }
    Ok(Some((ordered, Vec::new())))
}
