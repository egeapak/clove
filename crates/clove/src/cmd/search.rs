//! `clove search` (T-S05): FTS5 search when an index is present, else a parallel
//! file-content scan. Both paths return the same JSON shape; `_meta.source`
//! distinguishes them, and both rank through `view::rank_search_hits` — title
//! hits, then labels, then body, tie-broken by `(priority, id)`.
//!
//! Caveat, still open: the FTS matches whole *tokens* while `rank_search_hits`
//! matches substrings, so the index path is a narrower prefilter and the two
//! paths differ for a needle that is not a whole token (`core` inside
//! `corepart`). See `docs/READ_PATH_ROADMAP.md` §6.1.

use std::collections::HashMap;

use clove_core::view::{rank_search_hits, SearchOrder};
use clove_core::OutputFormat;
use clove_types::{CloveError, CloveId, ItemFrontmatter};

use clove_ipc::{DaemonClient, SearchRequest};

use crate::cli::SearchArgs;
use crate::cmd::listing::{emit, objects_from_frontmatters, window, ListOpts};
use crate::context::{index_error, Ctx};

/// Whether the index may answer a search, freshening it in place if it is only
/// slightly behind.
///
/// `search` previously queried the index with no staleness check at all, so any
/// item created since the last reindex was silently missing from results — and
/// a schema change left the file *empty*, which read as "no matches" for every
/// query rather than "index unavailable". `open_or_rebuild` now repopulates
/// instead of leaving it empty, so that second failure is fixed at the source;
/// this gate still mirrors the list commands' (`cmd/index_read.rs`), including
/// the `auto_refresh` opt-out and the too-far-behind bail.
fn usable_index(ctx: &Ctx, deep: bool) -> Result<Option<clove_index::Index>, CloveError> {
    let Ok(mut index) = clove_index::Index::open_or_rebuild(&ctx.db_path, &ctx.issues_dir) else {
        return Ok(None); // a broken index is non-fatal
    };
    if !ctx.config.index.auto_refresh {
        // The repo opted out of inline refresh; an unverified index must not
        // answer a search, because "no rows" is indistinguishable from "no hits".
        return Ok(None);
    }
    let report = if deep {
        index.check_staleness(&ctx.issues_dir)
    } else {
        index.check_staleness_fast(&ctx.issues_dir)
    }
    .map_err(|e| index_error(e, &ctx.db_path))?;
    if report.change_count() > crate::cmd::index_read::STALE_REFRESH_LIMIT {
        return Ok(None); // too far behind to freshen inline
    }
    if !report.is_clean() {
        index
            .apply_staleness(&report, &ctx.issues_dir)
            .map_err(|e| index_error(e, &ctx.db_path))?;
    }
    Ok(Some(index))
}

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: SearchArgs,
    no_index: bool,
    deep: bool,
) -> Result<(), CloveError> {
    let text = args.text;
    // Same window contract as every other list command: no flag → default
    // cap, `--limit 0` → unlimited.
    let window = window(args.offset, args.limit);
    // Search's default is relevance, not `rank`; naming a field replaces the
    // whole key. Every path below re-ranks locally over the full items (the FTS
    // is a candidate prefilter, not the ranker), so all three agree.
    let order = SearchOrder::parse(args.sort.as_deref(), args.desc.then_some("desc"))?;
    let fields = args.fields.as_deref().map(crate::item_json::parse_fields);

    // Daemon fast path: the daemon runs the FTS over its hot index and returns
    // matched ids; we still read those files for full detail, so the output is
    // identical to the local index path bar `_meta.source = "daemon"`. The
    // daemon is asked for ALL matches (the window is applied after ranking,
    // exactly like the local index path) — truncating inside its SQL would cut
    // by `(priority, topo, id)` before title matches are ranked first.
    if let Some(ids) = search_via_daemon(ctx, no_index, &text) {
        let items = ids
            .iter()
            .filter_map(|id| CloveId::new(id).ok())
            .filter_map(|id| ctx.store.get(&id).ok())
            .collect();
        let ranks = ranks_if_needed(ctx, order)?;
        let ordered = frontmatters_of(rank_search_hits(items, &text, order, &ranks));
        let objects = objects_from_frontmatters(&ordered);
        let total = objects.len();
        emit(
            format,
            objects,
            ListOpts {
                total,
                window,
                fields: fields.as_deref(),
                compact: args.compact,
                source: "daemon",
                sort: order.reported_sort(),
                dir: order.dir_str(),
                // `search` takes no field filters, so it echoes none — an empty
                // `filters` object would advertise a surface it does not have.
                filters: None,
                warnings: Vec::new(),
            },
        );
        return Ok(());
    }

    let (ordered, source) = if !no_index && ctx.db_path.exists() {
        match usable_index(ctx, deep)? {
            Some(index) => {
                // `None` limit: the FTS returns every candidate, so its own
                // ORDER BY never truncates and the local re-rank below decides
                // the result order. (The clause still has to be right — see
                // `clove_index::query::order_by_sql`.)
                let rows = index
                    .search(&text, &clove_core::view::Order::default(), None)
                    .map_err(|e| index_error(e, &ctx.db_path))?;
                // The FTS narrows the candidate set; the shared classifier does
                // the ranking, over the full items (it needs labels and body,
                // which the row does not carry).
                let mut items = Vec::new();
                for row in &rows {
                    if let Ok(id) = CloveId::new(&row.id) {
                        if let Ok(item) = ctx.store.get(&id) {
                            items.push(item);
                        }
                    }
                }
                let ranks = ranks_if_needed(ctx, order)?;
                (
                    frontmatters_of(rank_search_hits(items, &text, order, &ranks)),
                    "index",
                )
            }
            // A broken, stale, or freshly-rebuilt (empty) index is non-fatal:
            // fall back to files.
            None => (file_search(ctx, &text, order)?, "files"),
        }
    } else {
        (file_search(ctx, &text, order)?, "files")
    };

    let objects = objects_from_frontmatters(&ordered);
    let total = objects.len();
    emit(
        format,
        objects,
        ListOpts {
            total,
            window,
            fields: fields.as_deref(),
            compact: args.compact,
            source,
            sort: order.reported_sort(),
            dir: order.dir_str(),
            filters: None,
            warnings: Vec::new(),
        },
    );
    Ok(())
}

/// The topological ranks, but only when the requested order actually reads them
/// (`--sort rank`). Relevance and every other field key off the frontmatter
/// alone, so a search does not pay for a whole-store graph build it will not use.
fn ranks_if_needed(ctx: &Ctx, order: SearchOrder) -> Result<HashMap<CloveId, usize>, CloveError> {
    if !order.needs_ranks() {
        return Ok(HashMap::new());
    }
    // Ranks are a property of the *whole* graph, so they cannot be derived from
    // the matched subset — this scans the store even on the index path.
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    Ok(crate::cmd::listing::ranks_of(&frontmatters).1)
}

/// Try the daemon's FTS, returning ALL matched ids in rank order (the CLI
/// re-ranks and applies the page limit). `None` (→ local path) for
/// `--no-index` or when no daemon is live.
fn search_via_daemon(ctx: &Ctx, no_index: bool, text: &str) -> Option<Vec<String>> {
    if no_index {
        return None;
    }
    let clove_dir = ctx.issues_dir.parent()?;
    let mut client = DaemonClient::probe(clove_dir)?;
    client
        .search(SearchRequest {
            text: text.to_owned(),
            limit: None,
        })
        .ok()
}

/// Parallel substring scan over file content (the no-index fallback).
///
/// Matching and ranking are `view::rank_search_hits`, the same function
/// `ops::search` (MCP, web) and the index path above use. This path previously
/// had its own predicate that missed label matches entirely, and its own
/// two-class ranking where the shared one has three.
fn file_search(
    ctx: &Ctx,
    text: &str,
    order: SearchOrder,
) -> Result<Vec<ItemFrontmatter>, CloveError> {
    let (items, _errors) = ctx.store.scan()?;
    // This path already holds the whole store, so `--sort rank` builds its graph
    // from what is in hand rather than re-scanning through `ranks_if_needed`.
    let ranks = if order.needs_ranks() {
        let frontmatters: Vec<ItemFrontmatter> =
            items.iter().map(|i| i.frontmatter.clone()).collect();
        crate::cmd::listing::ranks_of(&frontmatters).1
    } else {
        HashMap::new()
    };
    Ok(frontmatters_of(rank_search_hits(
        items, text, order, &ranks,
    )))
}

/// Drop the bodies once ranking (which needs them) is done.
fn frontmatters_of(items: Vec<clove_types::Item>) -> Vec<ItemFrontmatter> {
    items.into_iter().map(|item| item.frontmatter).collect()
}
