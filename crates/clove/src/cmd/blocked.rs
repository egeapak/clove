//! `clove blocked` (T-CLI10): items blocked by open or missing dependencies.
//!
//! A thin adapter over [`clove_engine::Engine`], which owns the tiering. Two
//! things used to live here and no longer do:
//!
//! - the local re-sort of the daemon's ids, a second implementation of
//!   `view::Order` that could only fake `rank` (it had no topological ranks);
//!   the sort rides `GraphRequest::Blocked` and the daemon applies it.
//! - the `blocked_by` decoration, which is now part of the engine's answer —
//!   derived from each item's own dependency closure via `ops::graph_terms`, so
//!   it is O(page) rather than a second whole-store graph build, and it is the
//!   same helper `clove show` and the MCP tools use.
//!
//! `blocked` also gained an **index tier** here (read-path roadmap §5): it was
//! the one list that could not answer from SQL even with a hot index, because
//! `clove_index` had no blocked query. It has one now — the exact complement of
//! the `ready` clause — so a repo with an index but no daemon no longer pays a
//! whole-store file scan for `clove blocked`.

use clove_core::OutputFormat;
use clove_engine::Projection;
use clove_types::CloveError;

use crate::cli::FilterArgs;
use crate::cmd::listing::{emit, objects_from_answer, window, ListOpts};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    _quiet: bool,
    no_index: bool,
) -> Result<(), CloveError> {
    let filters = args.filters()?;
    let order = args.order()?;
    let fields = args.fields.as_deref().map(parse_fields);
    let window = window(args.offset, args.limit);

    // `blocked` has no lean projection — `blocked_by` is the whole point of the
    // list and no lean row carries it — so the engine always hydrates the page.
    let answer = ctx
        .engine(no_index, false)
        .blocked(&filters, order, window, Projection::Full)?;

    emit(
        format,
        objects_from_answer(&answer),
        ListOpts {
            total: answer.total,
            window,
            fields: fields.as_deref(),
            compact: args.compact,
            source: answer.source.as_str(),
            sort: order.field.as_str(),
            dir: order.dir_str(),
            filters: Some(&filters),
            warnings: answer.warnings,
        },
    );
    Ok(())
}
