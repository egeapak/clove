//! `clove dep add|rm|tree|cycle` (T-CLI08, T-CLI09).

use clove_core::graph::{render_dep_tree_human, DepTreeNode};
use clove_core::{CloveError, GraphStore, OutputFormat};
use serde_json::{json, Map, Value};

use crate::cli::{DepAction, DepCycleArgs, DepTreeArgs};
use crate::context::Ctx;
use crate::exit::ExitCode;
use crate::item_json::print_item;
use crate::output::print_json_success;
use crate::util::{now_seconds, parse_id};

pub fn run(ctx: &Ctx, format: OutputFormat, action: DepAction) -> Result<ExitCode, CloveError> {
    match action {
        DepAction::Add { id, dep_id } => add(ctx, format, &id, &dep_id).map(|_| ExitCode::Success),
        DepAction::Rm { id, dep_id } => rm(ctx, format, &id, &dep_id).map(|_| ExitCode::Success),
        DepAction::Tree(args) => tree(ctx, format, args).map(|_| ExitCode::Success),
        DepAction::Cycle(args) => cycle(ctx, format, args),
    }
}

fn add(ctx: &Ctx, format: OutputFormat, id_s: &str, dep_s: &str) -> Result<(), CloveError> {
    let id = parse_id(id_s)?;
    let dep = parse_id(dep_s)?;

    // Validation pipeline (DESIGN §5.4), in order.
    if !ctx.store.exists(&id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    if !ctx.store.exists(&dep) {
        return Err(CloveError::NotFound {
            id: dep.to_string(),
        });
    }
    if id == dep {
        return Err(CloveError::SelfDependency { id: id.to_string() });
    }

    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    if graph.check_would_cycle(&id, &dep) {
        return Err(CloveError::DependencyCycle {
            from: id.to_string(),
            to: dep.to_string(),
            cycle: vec![id.to_string(), dep.to_string()],
        });
    }

    let mut item = ctx.store.get(&id)?;
    if item.frontmatter.deps.contains(&dep) {
        return Err(CloveError::DependencyExists {
            from: id.to_string(),
            to: dep.to_string(),
        });
    }
    item.frontmatter.deps.push(dep);
    item.frontmatter.deps.sort();
    item.frontmatter.deps.dedup();
    let saved = ctx.store.update(&item, now_seconds())?;
    print_item(format, &saved, Map::new());
    Ok(())
}

fn rm(ctx: &Ctx, format: OutputFormat, id_s: &str, dep_s: &str) -> Result<(), CloveError> {
    let id = parse_id(id_s)?;
    let dep = parse_id(dep_s)?;
    let mut item = ctx.store.get(&id)?;
    item.frontmatter.deps.retain(|d| d != &dep);
    let saved = ctx.store.update(&item, now_seconds())?;
    print_item(format, &saved, Map::new());
    Ok(())
}

fn tree(ctx: &Ctx, format: OutputFormat, args: DepTreeArgs) -> Result<(), CloveError> {
    let id = parse_id(&args.id)?;
    if !ctx.store.exists(&id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let depth = if args.full { usize::MAX } else { args.depth };
    let root = graph
        .dep_tree(&id, depth)
        .ok_or_else(|| CloveError::NotFound { id: id.to_string() })?;

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => {
            let data = if args.flat {
                let mut flat = Vec::new();
                flatten(&root, 0, &mut flat);
                Value::Array(flat)
            } else {
                tree_to_json(&root)
            };
            print_json_success(data, json!({ "warnings": [] }));
        }
        OutputFormat::Human => print!("{}", render_dep_tree_human(&root)),
    }
    Ok(())
}

fn cycle(ctx: &Ctx, format: OutputFormat, args: DepCycleArgs) -> Result<ExitCode, CloveError> {
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let cycles = graph.all_cycles();

    let arrays: Vec<Value> = cycles
        .iter()
        .map(|c| Value::Array(c.iter().map(|id| json!(id.as_str())).collect()))
        .collect();

    match format {
        OutputFormat::Json | OutputFormat::Jsonl => print_json_success(
            Value::Array(arrays),
            json!({ "warnings": [], "count": cycles.len() }),
        ),
        OutputFormat::Human => {
            if cycles.is_empty() {
                println!("no cycles");
            } else {
                for c in &cycles {
                    let ids: Vec<&str> = c.iter().map(|id| id.as_str()).collect();
                    println!("{}", ids.join(" → "));
                }
            }
        }
    }

    if args.fail_on_cycle && !cycles.is_empty() {
        Ok(ExitCode::Cycle)
    } else {
        Ok(ExitCode::Success)
    }
}

fn tree_to_json(node: &DepTreeNode) -> Value {
    json!({
        "id": node.id.as_str(),
        "title": node.title,
        "status": node.status.as_str(),
        "ready": node.ready,
        "cycle_ref": node.cycle_ref,
        "children": node.children.iter().map(tree_to_json).collect::<Vec<_>>(),
    })
}

fn flatten(node: &DepTreeNode, depth: usize, out: &mut Vec<Value>) {
    out.push(json!({
        "id": node.id.as_str(),
        "title": node.title,
        "status": node.status.as_str(),
        "ready": node.ready,
        "cycle_ref": node.cycle_ref,
        "depth": depth,
    }));
    for child in &node.children {
        flatten(child, depth + 1, out);
    }
}
