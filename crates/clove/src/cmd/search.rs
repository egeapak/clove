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

use clove_core::view::SearchOrder;
use clove_core::OutputFormat;
use clove_types::CloveError;

use crate::cli::SearchArgs;
use crate::cmd::listing::{emit, objects_from_answer, window, ListOpts};
use crate::context::Ctx;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: SearchArgs,
    _no_index: bool,
    _deep: bool,
) -> Result<(), CloveError> {
    let text = args.text;
    // Same window contract as every other list command: no flag -> default
    // cap, `--limit 0` -> unlimited.
    let window = window(args.offset, args.limit);
    // Search's default is relevance, not `rank`; naming a field replaces the
    // whole key.
    let order = SearchOrder::parse(args.sort.as_deref(), args.desc.then_some("desc"))?;
    let fields = args.fields.as_deref().map(crate::item_json::parse_fields);

    // `Engine::search` has exactly one tier, so `--no-index`/`--deep` have
    // nothing to select and the engine is built without them.
    let answer = ctx.engine(true, false).search(&text, order, window)?;

    emit(
        format,
        objects_from_answer(&answer),
        ListOpts {
            total: answer.total,
            window,
            fields: fields.as_deref(),
            compact: args.compact,
            // Always `files`, and honestly so: an index or a live daemon cannot
            // change this command's answer, so there is no second source to
            // report.
            source: answer.source.as_str(),
            sort: order.reported_sort(),
            dir: order.dir_str(),
            // `search` takes no field filters, so it echoes none — an empty
            // `filters` object would advertise a surface it does not have.
            filters: None,
            warnings: answer.warnings,
        },
    );
    Ok(())
}
