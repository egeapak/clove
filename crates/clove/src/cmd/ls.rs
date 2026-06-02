//! `clove ls` (T-CLI11): list items with optional filters.

use clove_core::{CloveError, OutputFormat};
use clove_index::QueryMode;

use crate::cli::FilterArgs;
use crate::cmd::index_read::list_via_index;
use crate::cmd::listing::{emit, ranks_of, sort_by_priority_topo, Filters, ListOpts};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    no_index: bool,
) -> Result<(), CloveError> {
    let filters = Filters::parse(
        args.status.as_deref(),
        args.item_type.as_deref(),
        args.label.as_deref(),
        args.assignee.as_deref(),
        args.priority,
    )?;
    let fields = args.fields.as_deref().map(parse_fields);

    // Index fast path: the DB does the filtering and ordering.
    if let Some((ordered, warnings)) = list_via_index(ctx, no_index, QueryMode::List, &filters)? {
        let total = ordered.len();
        emit(
            format,
            &ordered,
            ListOpts {
                total,
                offset: args.offset.unwrap_or(0),
                limit: args.limit,
                fields: fields.as_deref(),
                source: "index",
                warnings,
            },
        );
        return Ok(());
    }

    // File-scan fallback.
    let (mut frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (_graph, ranks) = ranks_of(&frontmatters);
    frontmatters.retain(|fm| filters.matches(fm));
    sort_by_priority_topo(&mut frontmatters, &ranks);

    let total = frontmatters.len();
    emit(
        format,
        &frontmatters,
        ListOpts {
            total,
            offset: args.offset.unwrap_or(0),
            limit: args.limit,
            fields: fields.as_deref(),
            source: "files",
            warnings: Vec::new(),
        },
    );
    Ok(())
}
