//! `clove search` (T-S05): FTS5 search when an index is present, else a parallel
//! file-content scan. Both paths return the same JSON shape; `_meta.source`
//! distinguishes them. Title matches are ranked ahead of body-only matches.

use clove_core::{CloveError, CloveId, ItemFrontmatter, OutputFormat};

use crate::cli::SearchArgs;
use crate::cmd::listing::{emit, ListOpts};
use crate::context::{index_error, Ctx};

pub fn run(
    ctx: &Ctx,
    format: OutputFormat,
    args: SearchArgs,
    no_index: bool,
) -> Result<(), CloveError> {
    let text = args.text;

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

    let total = ordered.len();
    emit(
        format,
        &ordered,
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
