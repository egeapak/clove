//! `clove show` (T-CLI04).
//!
//! Reads the item file and its comment dir, then derives `ready`/`blocked_by`
//! from the item's own dependency closure via `ops::graph_terms` — the same
//! helper the MCP tool and the daemon RPC use. That used to be a whole-store
//! scan here, which is why the fields were gated behind `--verbose`; they are
//! now always computed.

use clove_core::{list_comments, OutputFormat};
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::cli::ShowArgs;
use crate::context::Ctx;
use crate::item_json::{item_object, parse_fields, project};
use crate::output::print_json_success;
use crate::util::parse_id;

pub fn run(ctx: &Ctx, format: OutputFormat, args: ShowArgs) -> Result<(), CloveError> {
    let id = parse_id(&args.id)?;
    let item = ctx.store.get(&id)?;
    let comment_count = list_comments(&ctx.issues_dir, &id)
        .map(|c| c.len())
        .unwrap_or(0);

    let fields = args.fields.as_deref().map(parse_fields);

    let mut obj = item_object(&item);
    obj.insert("body".to_owned(), json!(item.body));
    obj.insert("comment_count".to_owned(), json!(comment_count));

    // `ready`/`blocked_by` are always computed now. They used to be gated behind
    // `--verbose` (with a "pass --verbose" warning and `null` placeholders)
    // purely because deriving them meant scanning and parsing the whole store;
    // `ops::graph_terms` answers from the item's own closure instead, so the
    // gate bought nothing but a degraded default. Same helper as the MCP tool
    // and the daemon RPC — one implementation, not three.
    let (ready, blocked_by) = clove_core::ops::graph_terms(&ctx.store, &item.frontmatter)?;
    obj.insert("ready".to_owned(), json!(ready));
    obj.insert("blocked_by".to_owned(), json!(blocked_by));

    let projected = match &fields {
        Some(f) => project(obj, f),
        None => obj,
    };

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_success(Value::Object(projected), json!({ "warnings": [] }))
        }
        OutputFormat::Human => print_human(&item, comment_count, &projected),
    }
    Ok(())
}

fn print_human(
    item: &clove_types::Item,
    comment_count: usize,
    obj: &serde_json::Map<String, Value>,
) {
    let fm = &item.frontmatter;
    println!("{}  {}", fm.id.as_str(), fm.title);
    println!("  status:   {}", fm.status.as_str());
    println!("  type:     {}", fm.item_type.as_str());
    println!("  priority: {}", fm.priority.get());
    if let Some(a) = &fm.assignee {
        println!("  assignee: {a}");
    }
    if !fm.labels.is_empty() {
        println!("  labels:   {}", fm.labels.join(", "));
    }
    if !fm.deps.is_empty() {
        let deps: Vec<&str> = fm.deps.iter().map(|d| d.as_str()).collect();
        println!("  deps:     {}", deps.join(", "));
    }
    if let Some(ready) = obj.get("ready").and_then(Value::as_bool) {
        println!("  ready:    {ready}");
    }
    println!("  comments: {comment_count}");
    if !item.body.trim().is_empty() {
        println!("\n{}", item.body.trim_end());
    }
}
