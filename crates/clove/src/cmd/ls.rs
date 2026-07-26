//! `clove ls` (T-CLI11): list items with optional filters.

use clove_core::OutputFormat;
use clove_index::QueryMode;
use clove_types::CloveError;

use crate::cli::FilterArgs;
use crate::cmd::index_read::{list_via_daemon, list_via_index};
use crate::cmd::listing::{
    emit, lean_can_serve, objects_from_frontmatters, objects_from_lean_rows, ranks_of, window,
    Filters, ListOpts,
};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    no_index: bool,
    deep: bool,
) -> Result<(), CloveError> {
    let filters = Filters::parse(
        args.status.as_deref(),
        args.item_type.as_deref(),
        args.label.as_deref(),
        args.assignee.as_deref(),
        args.priority,
    )?;
    let order = args.order()?;
    let fields = args.fields.as_deref().map(parse_fields);
    let window = window(args.offset, args.limit);

    // Daemon fast path: a running daemon serves the lean projection from its hot
    // index (the CLI skips its own staleness scan — the daemon owns freshness).
    let lean_ok = lean_can_serve(fields.as_deref());
    if let Some((objects, total, warnings)) = lean_ok
        .then(|| list_via_daemon(ctx, no_index, QueryMode::List, &filters, order, window))
        .flatten()
    {
        emit(
            format,
            objects,
            ListOpts {
                total,
                window,
                fields: fields.as_deref(),
                compact: args.compact,
                source: "daemon",
                sort: order.field.as_str(),
                dir: order.dir_str(),
                warnings,
            },
        );
        return Ok(());
    }

    // Index fast path: the DB serves the lean projection directly.
    if let Some((rows, total, warnings)) = match lean_ok {
        true => list_via_index(
            ctx,
            no_index,
            deep,
            QueryMode::List,
            &filters,
            order,
            window,
        )?,
        false => None,
    } {
        emit(
            format,
            objects_from_lean_rows(&rows),
            ListOpts {
                total,
                window,
                fields: fields.as_deref(),
                compact: args.compact,
                source: "index",
                sort: order.field.as_str(),
                dir: order.dir_str(),
                warnings,
            },
        );
        return Ok(());
    }

    // File-scan fallback (full frontmatter objects).
    let (mut frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (_graph, ranks) = ranks_of(&frontmatters);
    frontmatters.retain(|fm| filters.matches(fm));
    order.apply(&mut frontmatters, &ranks);

    let objects = objects_from_frontmatters(&frontmatters);
    let total = objects.len();
    emit(
        format,
        objects,
        ListOpts {
            total,
            window,
            fields: fields.as_deref(),
            compact: args.compact,
            source: "files",
            sort: order.field.as_str(),
            dir: order.dir_str(),
            warnings: Vec::new(),
        },
    );
    Ok(())
}
