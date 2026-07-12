//! `clove new` (T-CLI03): create an item.
//!
//! Thin shim over `clove_core::ops::create` — the same entry point the daemon,
//! web, and MCP surfaces use — so type/priority/label parsing and referential
//! validation live in exactly one place (the unified-write-path rule).

use chrono::Utc;
use clove_core::ops::NewSpec;
use clove_core::OutputFormat;
use clove_types::CloveError;
use serde_json::json;

use crate::cli::NewArgs;
use crate::context::Ctx;
use crate::output::print_json_success;

pub fn run(ctx: &Ctx, format: OutputFormat, args: NewArgs) -> Result<(), CloveError> {
    let spec = NewSpec {
        title: args.title,
        item_type: args.item_type,
        priority: args.priority,
        labels: args.labels,
        deps: args.deps,
        parent: args.parent,
        assignee: args.assignee,
        body: args.body,
    };

    let value = clove_core::ops::create(
        &ctx.store,
        &ctx.config.id_prefix,
        ctx.config.default_type,
        spec,
        Utc::now(),
    )?;

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_success(value, json!({ "warnings": [] }))
        }
        OutputFormat::Human => println!(
            "{}  {}",
            value["id"].as_str().unwrap_or_default(),
            value["path"].as_str().unwrap_or_default()
        ),
    }
    Ok(())
}
