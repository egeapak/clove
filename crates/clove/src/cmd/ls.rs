//! `clove ls` (T-CLI11): list items with optional filters.
//!
//! A thin adapter: parse the flags, ask [`clove_engine::Engine`] (which owns the
//! daemon → index → files cascade), render. The three-branch cascade that used
//! to live here — duplicated, with slightly different fallback conditions, in
//! `ready`/`blocked`/`query` too — is read-path roadmap §4.

use clove_core::OutputFormat;
use clove_engine::Projection;
use clove_types::CloveError;

use crate::cli::FilterArgs;
use crate::cmd::listing::{emit, lean_can_serve, objects_from_answer, window, ListOpts};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    no_index: bool,
    deep: bool,
) -> Result<(), CloveError> {
    let filters = args.filters()?;
    let order = args.order()?;
    let fields = args.fields.as_deref().map(parse_fields);
    let window = window(args.offset, args.limit);

    // A `--fields` request reaching outside the lean row pins this to the file
    // scan rather than quietly arriving from a different projection.
    let projection = match lean_can_serve(fields.as_deref()) {
        true => Projection::Lean,
        false => Projection::Files,
    };
    let answer = ctx
        .engine(no_index, deep)
        .list(&filters, order, window, projection)?;

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
