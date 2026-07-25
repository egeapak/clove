//! `clove ready` (T-CLI10): items eligible to work on now.

use std::collections::HashMap;

use clove_core::OutputFormat;
use clove_index::QueryMode;
use clove_types::{CloveError, CloveId, ItemFrontmatter};

use crate::cli::FilterArgs;
use crate::cmd::index_read::{list_via_daemon, list_via_index};
use crate::cmd::listing::{
    emit, objects_from_frontmatters, objects_from_lean_rows, ranks_of, window, Filters, ListOpts,
};
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
    let filters = Filters::parse(
        args.status.as_deref(),
        args.item_type.as_deref(),
        args.label.as_deref(),
        args.assignee.as_deref(),
        args.priority,
    )?;
    let fields = args.fields.as_deref().map(parse_fields);
    let window = window(args.offset, args.limit);

    // Daemon fast path: a running daemon serves the ready set from its hot index.
    if let Some((objects, total, warnings)) =
        list_via_daemon(ctx, no_index, QueryMode::Ready, &filters, window)
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
                warnings,
            },
        );
        return Ok(());
    }

    // Index fast path: the ready SQL replaces the in-memory graph build.
    if let Some((rows, total, warnings)) =
        list_via_index(ctx, no_index, deep, QueryMode::Ready, &filters, window)?
    {
        emit(
            format,
            objects_from_lean_rows(&rows),
            ListOpts {
                total,
                window,
                fields: fields.as_deref(),
                compact: args.compact,
                source: "index",
                warnings,
            },
        );
        return Ok(());
    }

    // File-scan fallback: build the graph and compute the ready set.
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
    ordered.retain(|fm| filters.matches(fm));

    // Items excluded from `ready` because they reference missing dependencies.
    // They are not lost: `clove blocked` lists them, with the broken ids in
    // `blocked_by`.
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
            source: "files",
            warnings,
        },
    );
    Ok(())
}
