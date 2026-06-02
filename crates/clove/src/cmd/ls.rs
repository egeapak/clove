//! `clove ls` (T-CLI11): list items with optional filters.

use clove_core::{CloveError, OutputFormat};

use crate::cli::FilterArgs;
use crate::cmd::listing::{emit, ranks_of, sort_by_priority_topo, Filters, ListOpts};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(ctx: &Ctx, format: OutputFormat, args: FilterArgs) -> Result<(), CloveError> {
    let (mut frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let filters = Filters::parse(
        args.status.as_deref(),
        args.item_type.as_deref(),
        args.label.as_deref(),
        args.assignee.as_deref(),
        args.priority,
    )?;

    let (_graph, ranks) = ranks_of(&frontmatters);
    frontmatters.retain(|fm| filters.matches(fm));
    sort_by_priority_topo(&mut frontmatters, &ranks);

    let total = frontmatters.len();
    let fields = args.fields.as_deref().map(parse_fields);
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
