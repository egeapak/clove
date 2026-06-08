//! `clove show` (T-CLI04).
//!
//! Fast path (human, no graph fields): read only the item file and its comment
//! dir. Full path (JSON, `--verbose`, or `--fields` requesting `ready`/
//! `blocked_by`): scan all frontmatter and build the graph to compute them.

use clove_core::{list_comments, GraphStore, OutputFormat};
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
    let wants_graph = matches!(format, OutputFormat::Json | OutputFormat::Jsonl)
        || args.verbose
        || fields
            .as_ref()
            .map(|f| f.iter().any(|k| k == "ready" || k == "blocked_by"))
            .unwrap_or(false);

    let mut obj = item_object(&item);
    obj.insert("body".to_owned(), json!(item.body));
    obj.insert("comment_count".to_owned(), json!(comment_count));

    let mut warnings: Vec<String> = Vec::new();
    if wants_graph {
        let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
        let (graph, _dangling) = GraphStore::build(&frontmatters);
        let ready = graph.ready_items().contains(&id);
        let blocked_by: Vec<String> = graph
            .blocked_items()
            .into_iter()
            .find(|b| b.id == id)
            .map(|b| {
                b.blocking_deps
                    .iter()
                    .chain(b.dangling_deps.iter())
                    .map(|x| x.to_string())
                    .collect()
            })
            .unwrap_or_default();
        obj.insert("ready".to_owned(), json!(ready));
        obj.insert("blocked_by".to_owned(), json!(blocked_by));
    } else {
        obj.insert("ready".to_owned(), Value::Null);
        obj.insert("blocked_by".to_owned(), Value::Null);
        warnings.push("pass --verbose for ready/blocked_by".to_owned());
    }

    let projected = match &fields {
        Some(f) => project(obj, f),
        None => obj,
    };

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_success(Value::Object(projected), json!({ "warnings": warnings }))
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
