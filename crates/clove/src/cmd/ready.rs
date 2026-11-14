//! `clove ready` (T-CLI10): items eligible to work on now.
//!
//! A thin adapter over [`clove_engine::Engine`], which owns the tiering — see
//! `cmd::ls`.

use clove_core::OutputFormat;
use clove_types::CloveError;

use crate::cli::FilterArgs;
use crate::cmd::listing::{emit, list_projection, objects_from_answer, window, ListOpts};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    quiet: bool,
    no_index: bool,
    deep: bool,
) -> Result<(), CloveError> {
    let filters = args.filters()?;
    let order = args.order()?;
    let fields = args.fields.as_deref().map(parse_fields);
    let window = window(args.offset, args.limit);

    let projection = list_projection(format, fields.as_deref());
    let answer = ctx
        .engine(no_index, deep)
        .ready(&filters, order, window, projection)?;

    // Items excluded from `ready` because they reference missing dependencies.
    // They are not lost: `clove blocked` lists them, with the broken ids in
    // `blocked_by`. Only the file tier can see them — the index and daemon
    // answer the ready query in SQL and never build the dangling set — which is
    // why this is echoed rather than asserted.
    if !quiet && matches!(format, OutputFormat::Human) {
        for msg in &answer.warnings {
            eprintln!("warning: {msg}");
        }
    }

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
