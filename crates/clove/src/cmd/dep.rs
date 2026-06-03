//! `clove dep add|rm|tree|cycle` (T-CLI08, T-CLI09).

use clove_core::graph::{render_dep_tree_human, DepTreeNode};
use clove_core::{CloveError, CloveId, GraphStore, OutputFormat};
use clove_ipc::{DaemonClient, GraphRequest, GraphResponse};
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

    if would_cycle(ctx, &id, &dep)? {
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
    let depth = if args.full { usize::MAX } else { args.depth };
    // Daemon fast path: serve the tree from the daemon's cached graph.
    let root = match dep_tree_via_daemon(ctx, &id, depth) {
        Some(node_opt) => node_opt.ok_or_else(|| CloveError::NotFound { id: id.to_string() })?,
        None => {
            let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
            let (graph, _dangling) = GraphStore::build(&frontmatters);
            graph
                .dep_tree(&id, depth)
                .ok_or_else(|| CloveError::NotFound { id: id.to_string() })?
        }
    };

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
    // Daemon fast path: serve cycles from the daemon's cached graph.
    let cycles: Vec<Vec<String>> = match cycles_via_daemon(ctx) {
        Some(cycles) => cycles,
        None => {
            let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
            let (graph, _dangling) = GraphStore::build(&frontmatters);
            graph
                .all_cycles()
                .iter()
                .map(|c| c.iter().map(|id| id.to_string()).collect())
                .collect()
        }
    };

    let arrays: Vec<Value> = cycles
        .iter()
        .map(|c| Value::Array(c.iter().map(|id| json!(id)).collect()))
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
                    let ids: Vec<&str> = c.iter().map(|s| s.as_str()).collect();
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

// ---- Daemon routing (Tier 2) --------------------------------------------------

/// `.clove/` dir, or `None` if it cannot be located.
fn daemon_client(ctx: &Ctx) -> Option<DaemonClient> {
    DaemonClient::probe(ctx.issues_dir.parent()?)
}

/// Whether `from → to` would cycle. Uses the daemon's cached graph when alive,
/// else builds the graph locally.
fn would_cycle(ctx: &Ctx, from: &CloveId, to: &CloveId) -> Result<bool, CloveError> {
    if let Some(mut client) = daemon_client(ctx) {
        if let Ok(GraphResponse::WouldCycle { would }) = client.graph(GraphRequest::WouldCycle {
            from: from.to_string(),
            to: to.to_string(),
        }) {
            return Ok(would);
        }
    }
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    Ok(graph.check_would_cycle(from, to))
}

/// Daemon-served dependency tree. Outer `None` = no daemon (caller falls back);
/// inner `None` = daemon reports the root unknown.
fn dep_tree_via_daemon(ctx: &Ctx, root: &CloveId, depth: usize) -> Option<Option<DepTreeNode>> {
    let mut client = daemon_client(ctx)?;
    match client.graph(GraphRequest::Tree {
        root: root.to_string(),
        depth,
    }) {
        Ok(GraphResponse::Tree { node }) => Some(node),
        _ => None,
    }
}

/// Daemon-served cycles (member ids). `None` = no daemon.
fn cycles_via_daemon(ctx: &Ctx) -> Option<Vec<Vec<String>>> {
    let mut client = daemon_client(ctx)?;
    match client.graph(GraphRequest::Cycles) {
        Ok(GraphResponse::Cycles { cycles }) => Some(cycles),
        _ => None,
    }
}
