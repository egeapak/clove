//! `clove search` (T-S05): FTS5 search when an index is present, else a parallel
//! file-content scan. Both paths return the same JSON shape; `_meta.source`
//! distinguishes them. Title matches are ranked ahead of body-only matches.

use clove_core::{CloveError, CloveId, ItemFrontmatter, OutputFormat};

use clove_ipc::{DaemonClient, SearchRequest};

use crate::cli::SearchArgs;
use crate::cmd::listing::{emit, objects_from_frontmatters, ListOpts};
use crate::context::{index_error, Ctx};

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: SearchArgs,
    no_index: bool,
) -> Result<(), CloveError> {
    let text = args.text;

    // Daemon fast path: the daemon runs the FTS over its hot index and returns
    // matched ids; we still read those files for full detail, so the output is
    // identical to the local index path bar `_meta.source = "daemon"`.
    if let Some(ids) = search_via_daemon(ctx, no_index, &text, args.limit) {
        let frontmatters = ids
            .iter()
            .filter_map(|id| CloveId::new(id).ok())
            .filter_map(|id| ctx.store.get(&id).ok())
            .map(|item| item.frontmatter)
            .collect();
        let ordered = rank_title_first(frontmatters, &text);
        let objects = objects_from_frontmatters(&ordered);
        let total = objects.len();
        emit(
            format,
            objects,
            ListOpts {
                total,
                offset: 0,
                limit: args.limit,
                fields: None,
                source: "daemon",
                warnings: Vec::new(),
            },
        );
        return Ok(());
    }

    let (ordered, source) = if !no_index && ctx.db_path.exists() {
        match clove_index::Index::open_or_create(&ctx.db_path) {
            Ok(index) => {
                let rows = index
                    .search(&text, None)
                    .map_err(|e| index_error(e, &ctx.db_path))?;
                let mut frontmatters = Vec::new();
                for row in &rows {
                    if let Ok(id) = CloveId::new(&row.id) {
                        if let Ok(item) = ctx.store.get(&id) {
                            frontmatters.push(item.frontmatter);
                        }
                    }
                }
                (rank_title_first(frontmatters, &text), "index")
            }
            // A broken index is non-fatal: fall back to files.
            Err(_) => (file_search(ctx, &text)?, "files"),
        }
    } else {
        (file_search(ctx, &text)?, "files")
    };

    let objects = objects_from_frontmatters(&ordered);
    let total = objects.len();
    emit(
        format,
        objects,
        ListOpts {
            total,
            offset: 0,
            limit: args.limit,
            fields: None,
            source,
            warnings: Vec::new(),
        },
    );
    Ok(())
}

/// Try the daemon's FTS, returning matched ids in rank order. `None` (→ local
/// path) for `--no-index` or when no daemon is live.
fn search_via_daemon(
    ctx: &Ctx,
    no_index: bool,
    text: &str,
    limit: Option<usize>,
) -> Option<Vec<String>> {
    if no_index {
        return None;
    }
    let clove_dir = ctx.issues_dir.parent()?;
    let mut client = DaemonClient::probe(clove_dir)?;
    client
        .search(SearchRequest {
            text: text.to_owned(),
            limit,
        })
        .ok()
}

/// Parallel substring scan over file content (the no-index fallback).
fn file_search(ctx: &Ctx, text: &str) -> Result<Vec<ItemFrontmatter>, CloveError> {
    let needle = text.to_lowercase();
    let (items, _errors) = ctx.store.scan()?;
    let matched: Vec<ItemFrontmatter> = items
        .into_iter()
        .filter(|item| {
            item.frontmatter.title.to_lowercase().contains(&needle)
                || item.body.to_lowercase().contains(&needle)
        })
        .map(|item| item.frontmatter)
        .collect();
    Ok(rank_title_first(matched, text))
}

/// Stable order: title matches first, then `(priority, id)`.
fn rank_title_first(frontmatters: Vec<ItemFrontmatter>, text: &str) -> Vec<ItemFrontmatter> {
    let needle = text.to_lowercase();
    let mut keyed: Vec<(u8, ItemFrontmatter)> = frontmatters
        .into_iter()
        .map(|fm| {
            let rank = if fm.title.to_lowercase().contains(&needle) {
                0
            } else {
                1
            };
            (rank, fm)
        })
        .collect();
    keyed.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.priority.cmp(&b.1.priority))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    keyed.into_iter().map(|(_, fm)| fm).collect()
}
