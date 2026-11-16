//! `clove show` (T-CLI04).
//!
//! Reads the item file and its comment dir, then derives `ready`/`blocked_by`
//! from the item's own dependency closure via `ops::graph_terms` — the same
//! helper the MCP tool and the daemon RPC use. That used to be a whole-store
//! scan here, which is why the fields were gated behind `--verbose`; they are
//! now always computed. In human output `--verbose` adds the lines the default
//! view leaves out (`blocked_by`, the other graph edges, the external link,
//! and the timestamps), and `--fields` prints only the fields asked for.

use clove_core::{list_comments, OutputFormat};
use clove_plugin::outln;
use clove_types::CloveError;
use serde_json::{json, Value};

use crate::cli::ShowArgs;
use crate::context::Ctx;
use crate::item_json::{item_object, parse_fields, project};
use crate::output::print_json_success;
use crate::util::{parse_id, term_line, term_text};

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
    let projected = if args.compact {
        clove_core::view::compact_read(projected)
    } else {
        projected
    };

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            print_json_success(Value::Object(projected), json!({ "warnings": [] }))
        }
        OutputFormat::Human => match &fields {
            Some(fields) => print_fields(fields, &projected),
            None => print_human(&item, comment_count, &projected, args.verbose),
        },
    }
    Ok(())
}

/// `--fields` in human output: one `name: value` line per requested field
/// the item has, in the order asked (an unknown name is left out, as in JSON).
fn print_fields(fields: &[String], obj: &serde_json::Map<String, Value>) {
    for field in fields {
        let Some(value) = obj.get(field) else {
            continue;
        };
        match value {
            Value::String(text) if text.contains('\n') => {
                outln!("{field}:\n{}", term_text(text.trim_end()));
            }
            _ => outln!("{field}: {}", human_value(value)),
        }
    }
}

/// One JSON value as a single human line: `-` for null, a list joined by
/// `, `, a string as is.
fn human_value(value: &Value) -> String {
    match value {
        Value::Null => "-".to_owned(),
        Value::String(text) => term_line(text),
        Value::Array(values) if values.is_empty() => "-".to_owned(),
        Value::Array(values) => values
            .iter()
            .map(human_value)
            .collect::<Vec<_>>()
            .join(", "),
        other => term_line(&other.to_string()),
    }
}

fn print_human(
    item: &clove_types::Item,
    comment_count: usize,
    obj: &serde_json::Map<String, Value>,
    verbose: bool,
) {
    let fm = &item.frontmatter;
    outln!("{}  {}", fm.id.as_str(), term_line(&fm.title));
    outln!("  status:   {}", fm.status.as_str());
    outln!("  type:     {}", fm.item_type.as_str());
    outln!("  priority: {}", fm.priority.get());
    if let Some(a) = &fm.assignee {
        outln!("  assignee: {}", term_line(a));
    }
    if !fm.labels.is_empty() {
        outln!("  labels:   {}", term_line(&fm.labels.join(", ")));
    }
    if !fm.deps.is_empty() {
        let deps: Vec<&str> = fm.deps.iter().map(|d| d.as_str()).collect();
        outln!("  deps:     {}", deps.join(", "));
    }
    if let Some(ready) = obj.get("ready").and_then(Value::as_bool) {
        outln!("  ready:    {ready}");
    }
    if verbose {
        print_verbose(fm, obj);
    }
    outln!("  comments: {comment_count}");
    if !item.body.trim().is_empty() {
        outln!("\n{}", term_text(item.body.trim_end()));
    }
}

/// The `--verbose` lines: the graph edges, timestamps, and external link that
/// the default view leaves out. Empty fields are left out.
fn print_verbose(fm: &clove_types::ItemFrontmatter, obj: &serde_json::Map<String, Value>) {
    let ids = |ids: &[clove_types::CloveId]| {
        ids.iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(blocked_by) = obj.get("blocked_by").and_then(Value::as_array) {
        if !blocked_by.is_empty() {
            outln!(
                "  blocked:  {}",
                human_value(&Value::Array(blocked_by.clone()))
            );
        }
    }
    if let Some(parent) = &fm.parent {
        outln!("  parent:   {}", parent.as_str());
    }
    for (name, list) in [
        ("relates:", &fm.relates),
        ("duplicates:", &fm.duplicates),
        ("supersedes:", &fm.supersedes),
    ] {
        if !list.is_empty() {
            outln!("  {name:<9} {}", ids(list));
        }
    }
    if let Some(external_ref) = &fm.external_ref {
        let source = fm.source_system.as_deref().unwrap_or("-");
        outln!(
            "  external: {} {}",
            term_line(source),
            term_line(external_ref)
        );
    }
    outln!("  created:  {}", fm.created.to_rfc3339());
    outln!("  updated:  {}", fm.updated.to_rfc3339());
    if let Some(closed) = fm.closed {
        outln!("  closed:   {}", closed.to_rfc3339());
    }
}
