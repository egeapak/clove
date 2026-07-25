//! `clove blocked` (T-CLI10): items blocked by open or missing dependencies.

use std::collections::HashMap;

use clove_core::OutputFormat;
use clove_ipc::{DaemonClient, GraphRequest, GraphResponse};
use clove_types::{CloveError, CloveId, ItemFrontmatter};

use crate::cli::FilterArgs;
use crate::cmd::listing::{emit, ranks_of, sort_by_priority_topo, window, Filters, ListOpts};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    _quiet: bool,
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
    let window = window(args.offset, args.limit);

    // `blocked_by` is the whole point of this list, and the CLI used to omit it
    // while `ops::blocked` and the web API both emit it. `ops::graph_terms`
    // derives it from the item's own dependency closure, so this is O(page), not
    // a second whole-store graph build — and it is the same helper `clove show`
    // and the MCP tools use, so the ids cannot drift between surfaces.
    let with_blocked_by =
        |ctx: &Ctx, rows: &[ItemFrontmatter]| -> Vec<crate::cmd::listing::ListObject> {
            rows.iter()
                .map(|fm| {
                    let mut obj = crate::item_json::frontmatter_object(fm);
                    let blocked_by = clove_core::ops::graph_terms(&ctx.store, fm)
                        .map(|(_, by)| by)
                        .unwrap_or_default();
                    obj.insert("blocked_by".to_owned(), serde_json::json!(blocked_by));
                    obj
                })
                .collect()
        };

    // Daemon fast path: the daemon computes the blocked set + `(priority, topo,
    // id)` order from its cached graph and returns ordered ids; we read those
    // files for full detail (filters preserve the daemon's order). Same output as
    // the file path bar `_meta.source = "daemon"`.
    if let Some(ids) = blocked_via_daemon(ctx, no_index) {
        let ordered: Vec<ItemFrontmatter> = ids
            .iter()
            .filter_map(|id| CloveId::new(id).ok())
            .filter_map(|id| ctx.store.get(&id).ok())
            .map(|item| item.frontmatter)
            .filter(|fm| filters.matches(fm))
            .collect();
        let objects = with_blocked_by(ctx, &ordered);
        let total = objects.len();
        emit(
            format,
            objects,
            ListOpts {
                total,
                window,
                fields: fields.as_deref(),
                compact: args.compact,
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
        .filter_map(|b| by_id.get(&b.id).cloned())
        .collect();

    ordered.retain(|fm| filters.matches(fm));
    sort_by_priority_topo(&mut ordered, &ranks);

    let objects = with_blocked_by(ctx, &ordered);
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
            warnings: Vec::new(),
        },
    );
    Ok(())
}

/// Ask a running daemon for the blocked-item ids (ordered). `None` → local
/// path, forced by `--no-index` (the flag promises a file scan).
fn blocked_via_daemon(ctx: &Ctx, no_index: bool) -> Option<Vec<String>> {
    if no_index {
        return None;
    }
    let clove_dir = ctx.issues_dir.parent()?;
    let mut client = DaemonClient::probe(clove_dir)?;
    match // Always the full blocked set. The wire field predates the decision to
    // include dangling-only items by default; it stays on the protocol so no
    // version bump is needed, and every caller now passes `true`.
    client.graph(GraphRequest::Blocked {
            include_warnings: true,
        }) {
        Ok(GraphResponse::Blocked { ids }) => Some(ids),
        _ => None,
    }
}
