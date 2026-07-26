//! `clove blocked` (T-CLI10): items blocked by open or missing dependencies.

use std::collections::HashMap;

use clove_core::OutputFormat;
use clove_ipc::{DaemonClient, GraphRequest, GraphResponse};
use clove_types::{CloveError, CloveId, ItemFrontmatter};

use crate::cli::FilterArgs;
use crate::cmd::listing::{emit, ranks_of, window, ListOpts, Order};
use crate::context::Ctx;
use crate::item_json::parse_fields;

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: FilterArgs,
    _quiet: bool,
    no_index: bool,
) -> Result<(), CloveError> {
    let filters = args.filters()?;
    let order = args.order()?;
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

    // Daemon fast path: the daemon computes the blocked set from its cached
    // graph and orders it through the index's `ORDER BY` — the same clause
    // `ls`/`ready` run — then returns ordered ids; we read those files for full
    // detail. Filtering preserves the daemon's order, so the output matches the
    // file path bar `_meta.source = "daemon"`.
    //
    // The order used to be reapplied here, because the RPC carried no sort: the
    // ascending `rank` case kept the daemon's sequence, the descending case
    // reversed it, and every other field was re-sorted locally against an empty
    // rank map. That was a second implementation of `view::Order` living in a
    // command, and it could only fake `rank` (it had no topological ranks). The
    // sort now rides `GraphRequest::Blocked` and the daemon applies it.
    if let Some(ids) = blocked_via_daemon(ctx, no_index, order) {
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
                sort: order.field.as_str(),
                dir: order.dir_str(),
                filters: Some(&filters),
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
    order.apply(&mut ordered, &ranks);

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
            sort: order.field.as_str(),
            dir: order.dir_str(),
            filters: Some(&filters),
            warnings: Vec::new(),
        },
    );
    Ok(())
}

/// Ask a running daemon for the blocked-item ids, already in `order`. `None` →
/// local path, forced by `--no-index` (the flag promises a file scan).
///
/// The request no longer carries `include_warnings`: it was dead protocol —
/// nothing could set it and this call hard-coded `true` — so it went with the
/// v5 bump this ordering change needed anyway. Dangling-only items are in the
/// blocked set unconditionally (DESIGN §5.3); a broken reference is a data
/// problem to surface, not one to hide behind a flag.
fn blocked_via_daemon(ctx: &Ctx, no_index: bool, order: Order) -> Option<Vec<String>> {
    if no_index {
        return None;
    }
    let clove_dir = ctx.issues_dir.parent()?;
    let mut client = DaemonClient::probe(clove_dir)?;
    match client.graph(GraphRequest::Blocked { order }) {
        Ok(GraphResponse::Blocked { ids }) => Some(ids),
        _ => None,
    }
}
