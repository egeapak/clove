//! `clove ready` (T-CLI10): items eligible to work on now.

use std::collections::HashMap;

use clove_core::{CloveError, CloveId, ItemFrontmatter, OutputFormat};

use crate::cli::FilterArgs;
use crate::cmd::listing::{emit, ranks_of, Filters, ListOpts};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    quiet: bool,
) -> Result<(), CloveError> {
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let by_id: HashMap<CloveId, ItemFrontmatter> = frontmatters
        .iter()
        .cloned()
        .map(|fm| (fm.id.clone(), fm))
        .collect();

    let (graph, _ranks) = ranks_of(&frontmatters);
    // ready_items() is already ordered by (priority, topo rank, id).
    let mut ordered: Vec<ItemFrontmatter> = graph
        .ready_items()
        .iter()
        .filter_map(|id| by_id.get(id).cloned())
        .collect();

    let filters = Filters::parse(
        args.status.as_deref(),
        args.item_type.as_deref(),
        args.label.as_deref(),
        args.assignee.as_deref(),
        args.priority,
    )?;
    ordered.retain(|fm| filters.matches(fm));

    // Items excluded from `ready` because they reference missing dependencies.
    let mut warnings = Vec::new();
    let dangling: Vec<String> = frontmatters
        .iter()
        .filter(|fm| {
            graph
                .meta(&fm.id)
                .map(|m| m.has_dangling_deps())
                .unwrap_or(false)
        })
        .map(|fm| fm.id.to_string())
        .collect();
    if !dangling.is_empty() {
        let msg = format!(
            "{} item(s) excluded with dangling deps: {}",
            dangling.len(),
            dangling.join(", ")
        );
        if !quiet && matches!(format, OutputFormat::Human) {
            eprintln!("warning: {msg}");
        }
        warnings.push(msg);
    }

    let total = ordered.len();
    let fields = args.fields.as_deref().map(parse_fields);
    emit(
        format,
        &ordered,
        ListOpts {
            total,
            offset: args.offset.unwrap_or(0),
            limit: args.limit,
            fields: fields.as_deref(),
            source: "files",
            warnings,
        },
    );
    Ok(())
}
