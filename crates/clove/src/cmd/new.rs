//! `clove new` (T-CLI03): create an item.

use chrono::Utc;
use clove_core::{normalize_label, CloveError, NewItem, OutputFormat, Priority};
use serde_json::json;

use crate::cli::NewArgs;
use crate::context::{rel_to_root, Ctx};
use crate::output::print_json_success;
use crate::util::{parse_id, parse_priority, parse_type};

pub fn run(ctx: &Ctx, format: OutputFormat, args: NewArgs) -> Result<(), CloveError> {
    let item_type = match args.item_type.as_deref() {
        Some(t) => parse_type(t)?,
        None => ctx.config.default_type,
    };
    let priority = match args.priority {
        Some(p) => parse_priority(p)?,
        None => Priority::DEFAULT,
    };

    let mut labels = Vec::new();
    for raw in &args.labels {
        labels.push(normalize_label(raw)?);
    }
    labels.sort();
    labels.dedup();

    let mut deps = Vec::new();
    for raw in &args.deps {
        deps.push(parse_id(raw)?);
    }
    let parent = match args.parent.as_deref() {
        Some(p) => Some(parse_id(p)?),
        None => None,
    };

    let spec = NewItem {
        title: args.title,
        item_type,
        priority,
        labels,
        deps,
        parent,
        assignee: args.assignee,
        body: args.body.unwrap_or_default(),
    };

    let item = ctx.store.create(&ctx.config.id_prefix, spec, Utc::now())?;
    let id = item.frontmatter.id.clone();
    let rel = rel_to_root(&ctx.root, &ctx.store.path_for(&id));

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            json!({ "id": id.as_str(), "path": rel.as_str() }),
            json!({ "warnings": [] }),
        ),
        OutputFormat::Human => println!("{}  {}", id.as_str(), rel),
    }
    Ok(())
}
