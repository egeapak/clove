//! `clove search` (T-S05): a parallel file-content scan, always — there is no
//! index or daemon tier here, on purpose.
//!
//! Matching and ranking are `view::rank_search_hits`, the single definition also
//! used by `ops::search` (the `clove_search` MCP tool): title hits, then labels,
//! then body, tie-broken by `(priority, id)`.
//!
//! **Why no index tier** (read-path roadmap §6.1). Until index schema 6 this
//! command had three tiers — daemon FTS, local FTS, file scan — and the FTS
//! answered a *narrower* question than the file scan it was standing in for:
//! FTS5 matches whole tokens with ASCII-only case folding, while
//! `view::match_class` is `str::contains` over a full-Unicode lowercase. So
//! `clove search core` found a body reading `the corepart word` with
//! `--no-index` and missed it with an index; `clove search icode` found the
//! label `ünicode-tag` and the FTS never could, since FTS cannot match inside a
//! token at all. No FTS query is a superset of substring matching, so the choice
//! was between losing mid-word matching everywhere or dropping the prefilter.
//!
//! Dropping it costs less than it looks. The FTS only ever narrowed the
//! *candidate* set: this command then read every matched item file to rank it,
//! because ranking needs labels and body. Measured over a 10k-item store
//! (release build, warm cache): the file scan takes 62 ms for a needle matching
//! nothing and 216 ms for one matching nearly everything, where the index path
//! took 8 ms and 350 ms for the same two — the index won only for highly
//! selective needles and lost outright once more than a few percent of the store
//! matched. See `docs/READ_PATH_ROADMAP.md` §6.1.
//!
//! `--no-index`/`--deep` are accepted (they are global flags) and do nothing
//! here: there is one path, so there is nothing for them to select.

use std::collections::HashMap;

use clove_core::view::{rank_search_hits, SearchOrder};
use clove_core::OutputFormat;
use clove_types::{CloveError, ItemFrontmatter};

use crate::cli::SearchArgs;
use crate::cmd::listing::{emit, objects_from_frontmatters, window, ListOpts};
use crate::context::Ctx;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: SearchArgs,
    _no_index: bool,
    _deep: bool,
) -> Result<(), CloveError> {
    let text = args.text;
    // Same window contract as every other list command: no flag → default
    // cap, `--limit 0` → unlimited.
    let window = window(args.offset, args.limit);
    // Search's default is relevance, not `rank`; naming a field replaces the
    // whole key.
    let order = SearchOrder::parse(args.sort.as_deref(), args.desc.then_some("desc"))?;
    let fields = args.fields.as_deref().map(crate::item_json::parse_fields);

    let ordered = file_search(ctx, &text, order)?;
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
            // Always `files`, and honestly so: an index or a live daemon cannot
            // change this command's answer, so there is no second source to
            // report.
            source: "files",
            sort: order.reported_sort(),
            dir: order.dir_str(),
            // `search` takes no field filters, so it echoes none — an empty
            // `filters` object would advertise a surface it does not have.
            filters: None,
            warnings: Vec::new(),
        },
    );
    Ok(())
}

/// Parallel substring scan over file content.
///
/// Matching and ranking are `view::rank_search_hits`, the same function
/// `ops::search` (the MCP tool) uses. This path previously had its own predicate
/// that missed label matches entirely, and its own two-class ranking where the
/// shared one has three.
fn file_search(
    ctx: &Ctx,
    text: &str,
    order: SearchOrder,
) -> Result<Vec<ItemFrontmatter>, CloveError> {
    let (items, _errors) = ctx.store.scan()?;
    // This path holds the whole store, so `--sort rank` builds its graph from
    // what is in hand rather than re-scanning.
    let ranks = if order.needs_ranks() {
        let frontmatters: Vec<ItemFrontmatter> =
            items.iter().map(|i| i.frontmatter.clone()).collect();
        crate::cmd::listing::ranks_of(&frontmatters).1
    } else {
        HashMap::new()
    };
    Ok(rank_search_hits(items, text, order, &ranks)
        .into_iter()
        .map(|item| item.frontmatter)
        .collect())
}
