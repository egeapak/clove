//! `clove comment` / `clove comments` (T-CLI12).

use clove_core::comments::comment_author;
use clove_core::{add_comment, OutputFormat};
use clove_plugin::outln;
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::context::{rel_to_root, Ctx};
use crate::output::print_json_success;
use crate::util::parse_id;

pub fn add(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    message: &str,
    quiet: bool,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    if !ctx.store.exists(&id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let path = add_comment(&ctx.issues_dir, &id, &comment_author(&ctx.root), message)?;
    let rel = rel_to_root(&ctx.root, &path);
    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({ "id": id.as_str(), "path": rel.as_str() }),
            json!({ "warnings": [] }),
        ),
        OutputFormat::Human => {
            if !quiet {
                outln!("added comment to {}", id.as_str());
            }
        }
    }
    Ok(())
}

pub fn list(
    ctx: &Ctx,
    format: OutputFormat,
    id: &str,
    limit: Option<usize>,
    skip_newest: Option<usize>,
) -> Result<(), CloveError> {
    let id = parse_id(id)?;
    // Shared with the `clove_comments` MCP tool, so the two cannot drift on
    // which end `--limit` keeps — nor, through `Page`, on what `--limit 0`
    // means (unlimited, not "no comments").
    let window = clove_core::view::Page::new(
        skip_newest.unwrap_or(0),
        limit,
        clove_core::view::defaults::CLI_LIMIT,
    );
    let page = clove_core::ops::comments(&ctx.store, &id, window)?;
    let items = page["items"].as_array().cloned().unwrap_or_default();

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            // `data` is pinned to a bare array of comments by
            // `comment-list.json`, so the page counts ride in `_meta` (which the
            // schema leaves open) alongside every other list command's. Without
            // them a default-capped thread is indistinguishable from a short
            // one — the caller sees N comments and no way to learn there are
            // more.
            print_json_success(
                Value::Array(items),
                json!({
                    "total": page["total"],
                    "returned": page["returned"],
                    "skip_newest": page["skip_newest"],
                    "limit": page["limit"],
                    "warnings": [],
                }),
            );
        }
        OutputFormat::Human => {
            let total = page["total"].as_u64().unwrap_or(0);
            let returned = page["returned"].as_u64().unwrap_or(0);
            if returned < total {
                // The default cap must not truncate in silence: without this a
                // capped thread reads as the whole thread.
                outln!(
                    "showing {returned} of {total} comments \
                     (--limit 0 for all, --skip-newest N for older)\n"
                );
            }
            for c in &items {
                outln!(
                    "{}  {}",
                    c["timestamp"].as_str().unwrap_or_default(),
                    c["author"].as_str().unwrap_or_default()
                );
                outln!("{}\n", c["body"].as_str().unwrap_or_default().trim_end());
            }
        }
    }
    Ok(())
}
