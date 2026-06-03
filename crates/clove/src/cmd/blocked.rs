//! `clove blocked` (T-CLI10): items blocked by open or missing dependencies.

use std::collections::HashMap;

use clove_core::{CloveError, CloveId, ItemFrontmatter, OutputFormat};

use crate::cli::FilterArgs;
use crate::cmd::listing::{
    emit, objects_from_frontmatters, ranks_of, sort_by_priority_topo, Filters, ListOpts,
};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    _quiet: bool,
) -> Result<(), CloveError> {
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let by_id: HashMap<CloveId, ItemFrontmatter> = frontmatters
        .iter()
        .cloned()
        .map(|fm| (fm.id.clone(), fm))
        .collect();

    let (graph, ranks) = ranks_of(&frontmatters);
    let mut ordered: Vec<ItemFrontmatter> = graph
        .blocked_items()
        .into_iter()
        .filter(|b| args.include_warnings || !b.blocking_deps.is_empty())
        .filter_map(|b| by_id.get(&b.id).cloned())
        .collect();

    let filters = Filters::parse(
        args.status.as_deref(),
        args.item_type.as_deref(),
        args.label.as_deref(),
        args.assignee.as_deref(),
        args.priority,
    )?;
    ordered.retain(|fm| filters.matches(fm));
    sort_by_priority_topo(&mut ordered, &ranks);

    let fields = args.fields.as_deref().map(parse_fields);
    let objects = objects_from_frontmatters(&ordered);
    let total = objects.len();
    emit(
        format,
        objects,
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
