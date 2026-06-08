//! `clove blocked` (T-CLI10): items blocked by open or missing dependencies.

use std::collections::HashMap;

use clove_core::OutputFormat;
use clove_ipc::{DaemonClient, GraphRequest, GraphResponse};
use clove_types::{CloveError, CloveId, ItemFrontmatter};

use crate::cli::FilterArgs;
use crate::cmd::listing::{
    effective_limit, emit, objects_from_frontmatters, ranks_of, sort_by_priority_topo, Filters,
    ListOpts,
};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    _quiet: bool,
) -> Result<(), CloveError> {
    let filters = Filters::parse(
        args.status.as_deref(),
        args.item_type.as_deref(),
        args.label.as_deref(),
        args.assignee.as_deref(),
        args.priority,
    )?;
    let fields = args.fields.as_deref().map(parse_fields);

    // Daemon fast path: the daemon computes the blocked set + `(priority, topo,
    // id)` order from its cached graph and returns ordered ids; we read those
    // files for full detail (filters preserve the daemon's order). Same output as
    // the file path bar `_meta.source = "daemon"`.
    if let Some(ids) = blocked_via_daemon(ctx, args.include_warnings) {
        let ordered: Vec<ItemFrontmatter> = ids
            .iter()
            .filter_map(|id| CloveId::new(id).ok())
            .filter_map(|id| ctx.store.get(&id).ok())
            .map(|item| item.frontmatter)
            .filter(|fm| filters.matches(fm))
            .collect();
        let objects = objects_from_frontmatters(&ordered);
        let total = objects.len();
        emit(
            format,
            objects,
            ListOpts {
                total,
                offset: args.offset.unwrap_or(0),
                limit: effective_limit(args.limit),
                fields: fields.as_deref(),
                source: "daemon",
                warnings: Vec::new(),
            },
        );
        return Ok(());
    }

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

    ordered.retain(|fm| filters.matches(fm));
    sort_by_priority_topo(&mut ordered, &ranks);

    let objects = objects_from_frontmatters(&ordered);
    let total = objects.len();
    emit(
        format,
        objects,
        ListOpts {
            total,
            offset: args.offset.unwrap_or(0),
            limit: effective_limit(args.limit),
            fields: fields.as_deref(),
            source: "files",
            warnings: Vec::new(),
        },
    );
    Ok(())
}

/// Ask a running daemon for the blocked-item ids (ordered). `None` → local path.
fn blocked_via_daemon(ctx: &Ctx, include_warnings: bool) -> Option<Vec<String>> {
    let clove_dir = ctx.issues_dir.parent()?;
    let mut client = DaemonClient::probe(clove_dir)?;
    match client.graph(GraphRequest::Blocked { include_warnings }) {
        Ok(GraphResponse::Blocked { ids }) => Some(ids),
        _ => None,
    }
}
