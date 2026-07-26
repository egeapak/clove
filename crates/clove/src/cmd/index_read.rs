//! Index-accelerated read path (T-S06, DESIGN §6.4).
//!
//! When a fresh `index.db` is present, `ls`/`ready`/`query` resolve their
//! filtered, ordered result **directly from SQLite** as a lean projection (the
//! columns the list renders) — no per-item file read. A stale index is
//! transparently refreshed (≤ the threshold) or bypassed. Staleness uses the
//! fast O(readdir) check by default; `--deep` forces the thorough per-file pass.

use clove_index::{Index, ItemListRow, QueryMode};
use clove_ipc::{DaemonClient, QueryKind, QueryRequest};
use clove_types::CloveError;

use crate::cmd::listing::{objects_from_wire_rows, Filters, ListObject, Order};
use crate::context::{index_error, Ctx};

/// Above this many out-of-date items, skip the incremental refresh and fall back
/// to a file scan (DESIGN §6.4).
pub const STALE_REFRESH_LIMIT: usize = 20;

/// The index read result: the (already page-limited) lean rows, the full
/// unpaginated match count, and any warnings to surface.
pub type IndexList = (Vec<ItemListRow>, usize, Vec<String>);

/// The daemon read result: pre-shaped lean list objects, the full match count,
/// and warnings. Objects are built with the same [`objects_from_wire_rows`] /
/// `lean_object` builder as the index path, so daemon output is byte-identical
/// bar `_meta.source = "daemon"`.
pub type DaemonList = (Vec<ListObject>, usize, Vec<String>);

/// Try to satisfy a list/ready query via a running daemon (DESIGN §8.3/§8.4).
///
/// Probes liveness (50 ms, cleaning up a stale socket on the way); on a live
/// daemon, sends `QUERY` and returns its lean rows. Returns `None` — so the caller
/// falls back to [`list_via_index`] — for `--no-index`, no daemon, or any IPC
/// error. The daemon keeps its index fresh, so the CLI skips its own staleness
/// scan on this path.
pub fn list_via_daemon(
    ctx: &Ctx,
    no_index: bool,
    mode: QueryMode,
    filters: &Filters,
    order: Order,
    window: clove_core::view::Page,
) -> Option<DaemonList> {
    if no_index {
        return None;
    }
    let clove_dir = ctx.issues_dir.parent()?;
    let mut client = DaemonClient::probe(clove_dir)?;
    let kind = match mode {
        QueryMode::List => QueryKind::List,
        QueryMode::Ready => QueryKind::Ready,
    };
    let request = QueryRequest {
        kind,
        // The whole shared filter set rides the wire as one field, so a filter
        // added to `view::Filters` cannot be dropped in a per-field translation
        // on the way to the daemon — the failure that would look like the flag
        // simply not applying whenever `cloved` happens to be running.
        filters: filters.clone(),
        order,
        offset: window.offset,
        limit: window.limit,
    };
    match client.query_list(request) {
        Ok(resp) => Some((
            objects_from_wire_rows(&resp.rows),
            resp.total as usize,
            resp.warnings,
        )),
        Err(_) => None,
    }
}

/// Try to satisfy a list/ready query from the index.
///
/// `offset`/`limit` are the requested page; the SQL fetches only `offset + limit`
/// rows (all rows when `limit` is `None`) so a paginated `ls` steps just what it
/// needs, while `count_items` reports the full `total`.
///
/// Returns `Some((rows, total, warnings))` when the index was used (caller sets
/// `_meta.source = "index"`), or `None` to fall back to the file path. `None` is
/// returned for `--no-index`, a missing/broken index, or one too stale to refresh
/// cheaply.
pub fn list_via_index(
    ctx: &Ctx,
    no_index: bool,
    deep: bool,
    mode: QueryMode,
    filters: &Filters,
    order: Order,
    window: clove_core::view::Page,
) -> Result<Option<IndexList>, CloveError> {
    if no_index || !ctx.db_path.exists() {
        return Ok(None);
    }

    let mut index = match Index::open_or_rebuild(&ctx.db_path, &ctx.issues_dir) {
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

    // The exhaustive push-down: SQL for what SQLite can express, an in-memory
    // residue for what it cannot. Building the `Filter` by hand here is what
    // let `--label` and friends drift from the file path in the first place.
    let (mut filter, residue) = clove_index::push_down(filters);
    filter.mode = mode;
    // The SQL `ORDER BY` must match the file path's comparator exactly: with no
    // residue the limit is pushed into SQL, so a mismatched order returns the
    // wrong *rows*, not just the wrong sequence.
    filter.order = order;
    // `query_filtered` owns the limit/count decision, because a residue changes
    // both (it may not slice before filtering, and `COUNT(*)` is not the total).
    let (rows, total) = index
        .query_filtered(&filter, residue.as_ref(), window)
        .map_err(|e| index_error(e, &ctx.db_path))?;
    Ok(Some((rows, total, Vec::new())))
}
