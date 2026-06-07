//! The MCP tool registry: definitions (name + description + JSON Schema) and the
//! `call_tool` dispatcher.
//!
//! Each tool reuses the same `clove-core` engine and JSON-shaping helpers as the
//! CLI, so a tool's returned `data` is the CLI's JSON payload (minus the
//! envelope). Reads always re-scan the file store (the single source of truth),
//! so results track on-disk state; mutations go through `ItemStore`/`add_comment`
//! exactly as the CLI commands do.

use std::collections::HashMap;

use clove_core::graph::DepTreeNode;
use clove_core::{
    add_comment, compute_stats, list_comments, normalize_label, CloveError, CloveId, GraphStore,
    ItemFrontmatter, NewItem, Priority, StatsOptions,
};
use serde_json::{json, Map, Value};

use crate::cmd::edit::apply_assignments;
use crate::cmd::listing::{
    objects_from_frontmatters, ranks_of, sort_by_priority_topo, Filters,
};
use crate::cmd::status::set_status;
use crate::context::{rel_to_root, Ctx};
use crate::item_json::{frontmatter_object, item_object};
use crate::util::{now_seconds, parse_id, parse_priority, parse_status, parse_type};

/// Why a `tools/call` could not produce a result.
pub enum ToolError {
    /// No tool with that name exists (→ JSON-RPC `-32602`).
    Unknown,
    /// The arguments were missing/ill-typed (→ tool result `isError: true`).
    Args(String),
    /// The engine rejected the operation (→ tool result `isError: true`).
    Clove(CloveError),
}

impl From<CloveError> for ToolError {
    fn from(e: CloveError) -> Self {
        ToolError::Clove(e)
    }
}

/// The list of tool definitions advertised by `tools/list`.
pub fn tool_list() -> Vec<Value> {
    vec![
        tool(
            "clove_ready",
            "List work items that are ready to start now: open/in-progress items \
             whose hard dependencies are all closed and which have no dangling \
             dependencies. Ordered by (priority, dependency topology). This is the \
             primary 'what should I work on?' query.",
            schema(filter_props(), &[]),
        ),
        tool(
            "clove_blocked",
            "List work items blocked by open or missing dependencies, each with its \
             `blocked_by` ids. Ordered by (priority, topology).",
            schema(
                merge(
                    filter_props(),
                    json!({
                        "include_warnings": {
                            "type": "boolean",
                            "description": "Also include items blocked only by dangling (missing) deps."
                        }
                    }),
                ),
                &[],
            ),
        ),
        tool(
            "clove_list",
            "List work items with optional filters, ordered by (priority, topology, id).",
            schema(
                merge(
                    filter_props(),
                    json!({ "offset": { "type": "integer", "minimum": 0, "description": "Skip this many results." } }),
                ),
                &[],
            ),
        ),
        tool(
            "clove_show",
            "Show one work item in full: all fields, the Markdown body, comment \
             count, and computed `ready`/`blocked_by`.",
            schema(json!({ "id": id_prop() }), &["id"]),
        ),
        tool(
            "clove_search",
            "Full-text search over item titles, labels, and bodies (case-insensitive \
             substring; title matches rank first).",
            schema(
                json!({
                    "text": { "type": "string", "description": "The text to search for." },
                    "limit": limit_prop(),
                }),
                &["text"],
            ),
        ),
        tool(
            "clove_dep_tree",
            "Render the dependency tree rooted at an item (depth-limited), with \
             per-node status, `ready`, and `cycle_ref` markers.",
            schema(
                json!({
                    "id": id_prop(),
                    "depth": { "type": "integer", "minimum": 1, "description": "Maximum depth (default 5)." },
                }),
                &["id"],
            ),
        ),
        tool(
            "clove_stats",
            "Aggregate analytics for the repository: counts by status/type/priority/\
             assignee/label, ready/blocked/excluded/dangling totals, cycle count, \
             per-epic rollups, and created/closed throughput.",
            schema(
                json!({
                    "top": { "type": "integer", "minimum": 0, "description": "Cap assignee/label breakdowns to the N highest (default 10; 0 = no cap)." },
                    "no_epics": { "type": "boolean", "description": "Skip the per-epic completion rollup." },
                }),
                &[],
            ),
        ),
        tool(
            "clove_new",
            "Create a new work item. Returns its generated id and file path.",
            schema(
                json!({
                    "title": { "type": "string", "description": "The item title (required)." },
                    "type": type_prop(),
                    "priority": priority_prop(),
                    "labels": str_array_prop("Labels to attach (case-insensitive)."),
                    "deps": str_array_prop("Ids this item hard-depends on."),
                    "parent": { "type": "string", "description": "Parent item id (for epics)." },
                    "assignee": { "type": "string", "description": "Assignee." },
                    "body": { "type": "string", "description": "Markdown body." },
                }),
                &["title"],
            ),
        ),
        tool(
            "clove_status",
            "Change an item's status (open | in_progress | closed). Closing sets the \
             closed timestamp; reopening clears it.",
            schema(
                json!({ "id": id_prop(), "status": status_prop() }),
                &["id", "status"],
            ),
        ),
        tool(
            "clove_comment",
            "Append a comment to an item (author resolved from CLOVE_AUTHOR / \
             GIT_AUTHOR_EMAIL).",
            schema(
                json!({
                    "id": id_prop(),
                    "message": { "type": "string", "description": "The comment body." },
                }),
                &["id", "message"],
            ),
        ),
        tool(
            "clove_dep_add",
            "Add a hard dependency: `id` depends on `dep_id`. Rejects self-loops and \
             dependencies that would create a cycle.",
            schema(
                json!({
                    "id": id_prop(),
                    "dep_id": { "type": "string", "description": "The id `id` should depend on." },
                }),
                &["id", "dep_id"],
            ),
        ),
        tool(
            "clove_edit",
            "Edit one or more fields of an item in a single atomic write: status, \
             priority, type, title, assignee, and label add/remove.",
            schema(
                json!({
                    "id": id_prop(),
                    "status": status_prop(),
                    "priority": priority_prop(),
                    "type": type_prop(),
                    "title": { "type": "string", "description": "New title." },
                    "assignee": { "type": "string", "description": "New assignee (empty string clears it)." },
                    "add_labels": str_array_prop("Labels to add."),
                    "remove_labels": str_array_prop("Labels to remove."),
                }),
                &["id"],
            ),
        ),
    ]
}

/// Dispatch a `tools/call` by name. The returned `Value` is the tool's `data`
/// payload (object or array), which the transport wraps into an MCP tool result.
pub fn call_tool(ctx: &Ctx, name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "clove_ready" => ready(ctx, args),
        "clove_blocked" => blocked(ctx, args),
        "clove_list" => list(ctx, args),
        "clove_show" => show(ctx, args),
        "clove_search" => search(ctx, args),
        "clove_dep_tree" => dep_tree(ctx, args),
        "clove_stats" => stats(ctx, args),
        "clove_new" => new_item(ctx, args),
        "clove_status" => status(ctx, args),
        "clove_comment" => comment(ctx, args),
        "clove_dep_add" => dep_add(ctx, args),
        "clove_edit" => edit(ctx, args),
        _ => Err(ToolError::Unknown),
    }
}

// ---- Read tools ---------------------------------------------------------------

fn ready(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let filters = parse_filters(args)?;
    let limit = arg_limit(args, 50);
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let by_id: HashMap<CloveId, ItemFrontmatter> = frontmatters
        .iter()
        .cloned()
        .map(|fm| (fm.id.clone(), fm))
        .collect();
    let (graph, _ranks) = ranks_of(&frontmatters);
    let mut ordered: Vec<ItemFrontmatter> = graph
        .ready_items()
        .iter()
        .filter_map(|id| by_id.get(id).cloned())
        .collect();
    ordered.retain(|fm| filters.matches(fm));
    Ok(page(objects_from_frontmatters(&ordered), 0, limit))
}

fn blocked(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let filters = parse_filters(args)?;
    let include_warnings = arg_bool(args, "include_warnings");
    let limit = arg_limit(args, 50);
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let by_id: HashMap<CloveId, ItemFrontmatter> = frontmatters
        .iter()
        .cloned()
        .map(|fm| (fm.id.clone(), fm))
        .collect();
    let (graph, ranks) = ranks_of(&frontmatters);

    let mut rows: Vec<(ItemFrontmatter, Vec<String>, Vec<String>)> = graph
        .blocked_items()
        .into_iter()
        .filter(|b| include_warnings || !b.blocking_deps.is_empty())
        .filter_map(|b| {
            by_id.get(&b.id).cloned().map(|fm| {
                let blocking = b.blocking_deps.iter().map(CloveId::to_string).collect();
                let dangling = b.dangling_deps.iter().map(CloveId::to_string).collect();
                (fm, blocking, dangling)
            })
        })
        .collect();
    rows.retain(|(fm, _, _)| filters.matches(fm));
    rows.sort_by(|a, b| {
        a.0.priority
            .cmp(&b.0.priority)
            .then_with(|| rank_of(&ranks, &a.0.id).cmp(&rank_of(&ranks, &b.0.id)))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });

    let objects: Vec<Map<String, Value>> = rows
        .into_iter()
        .map(|(fm, blocking, dangling)| {
            let mut obj = frontmatter_object(&fm);
            let mut combined = blocking;
            combined.extend(dangling.iter().cloned());
            obj.insert("blocked_by".to_owned(), json!(combined));
            obj.insert("dangling_deps".to_owned(), json!(dangling));
            obj
        })
        .collect();
    Ok(page(objects, 0, limit))
}

fn list(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let filters = parse_filters(args)?;
    let limit = arg_limit(args, 50);
    let offset = arg_usize(args, "offset").unwrap_or(0);
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (_graph, ranks) = ranks_of(&frontmatters);
    let mut matched: Vec<ItemFrontmatter> =
        frontmatters.into_iter().filter(|fm| filters.matches(fm)).collect();
    sort_by_priority_topo(&mut matched, &ranks);
    Ok(page(objects_from_frontmatters(&matched), offset, limit))
}

fn show(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let id = parse_id(req_str(args, "id")?)?;
    let item = ctx.store.get(&id)?;
    let comment_count = list_comments(&ctx.issues_dir, &id)
        .map(|c| c.len())
        .unwrap_or(0);

    let mut obj = item_object(&item);
    obj.insert("body".to_owned(), json!(item.body));
    obj.insert("comment_count".to_owned(), json!(comment_count));

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
                .map(CloveId::to_string)
                .collect()
        })
        .unwrap_or_default();
    obj.insert("ready".to_owned(), json!(ready));
    obj.insert("blocked_by".to_owned(), json!(blocked_by));
    Ok(Value::Object(obj))
}

fn search(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let needle = req_str(args, "text")?.to_lowercase();
    let limit = arg_limit(args, 50);
    let items = ctx.store.list()?;

    // Rank: title matches (0) before label matches (1) before body matches (2).
    let mut hits: Vec<(u8, Map<String, Value>)> = Vec::new();
    for item in &items {
        let fm = &item.frontmatter;
        let in_title = fm.title.to_lowercase().contains(&needle);
        let in_label = fm.labels.iter().any(|l| l.to_lowercase().contains(&needle));
        let in_body = item.body.to_lowercase().contains(&needle);
        if in_title || in_label || in_body {
            let rank = if in_title {
                0
            } else if in_label {
                1
            } else {
                2
            };
            hits.push((rank, item_object(item)));
        }
    }
    hits.sort_by(|a, b| a.0.cmp(&b.0));
    let objects: Vec<Map<String, Value>> = hits.into_iter().map(|(_, o)| o).collect();
    Ok(page(objects, 0, limit))
}

fn dep_tree(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let id = parse_id(req_str(args, "id")?)?;
    if !ctx.store.exists(&id) {
        return Err(ToolError::Clove(CloveError::NotFound { id: id.to_string() }));
    }
    let depth = arg_usize(args, "depth").unwrap_or(5);
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let root = graph
        .dep_tree(&id, depth)
        .ok_or_else(|| ToolError::Clove(CloveError::NotFound { id: id.to_string() }))?;
    Ok(tree_to_json(&root))
}

fn stats(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let opts = StatsOptions {
        top: arg_usize(args, "top").unwrap_or(10),
        include_epics: !arg_bool(args, "no_epics"),
    };
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let report = compute_stats(&frontmatters, &graph, now_seconds(), opts);
    Ok(serde_json::to_value(&report).unwrap_or(Value::Null))
}

// ---- Mutation tools -----------------------------------------------------------

fn new_item(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let title = req_str(args, "title")?.to_owned();
    let item_type = match arg_str(args, "type") {
        Some(t) => parse_type(t)?,
        None => ctx.config.default_type,
    };
    let priority = match arg_u64(args, "priority") {
        Some(p) => parse_priority(p as u8)?,
        None => Priority::DEFAULT,
    };
    let mut labels = Vec::new();
    for raw in str_array(args, "labels") {
        labels.push(normalize_label(&raw)?);
    }
    labels.sort();
    labels.dedup();
    let mut deps = Vec::new();
    for raw in str_array(args, "deps") {
        deps.push(parse_id(&raw)?);
    }
    let parent = match arg_str(args, "parent") {
        Some(p) => Some(parse_id(p)?),
        None => None,
    };

    let spec = NewItem {
        title,
        item_type,
        priority,
        labels,
        deps,
        parent,
        assignee: arg_str(args, "assignee").map(str::to_owned),
        body: arg_str(args, "body").unwrap_or("").to_owned(),
    };
    let item = ctx.store.create(&ctx.config.id_prefix, spec, now_seconds())?;
    let id = item.frontmatter.id.clone();
    let rel = rel_to_root(&ctx.root, &ctx.store.path_for(&id));
    Ok(json!({ "id": id.as_str(), "path": rel.as_str() }))
}

fn status(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let id = parse_id(req_str(args, "id")?)?;
    let new_status = parse_status(req_str(args, "status")?)?;
    let mut item = ctx.store.get(&id)?;
    set_status(&mut item.frontmatter, new_status);
    let saved = ctx.store.update(&item, now_seconds())?;
    Ok(Value::Object(item_object(&saved)))
}

fn comment(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let id = parse_id(req_str(args, "id")?)?;
    if !ctx.store.exists(&id) {
        return Err(ToolError::Clove(CloveError::NotFound { id: id.to_string() }));
    }
    let message = req_str(args, "message")?;
    let path = add_comment(&ctx.issues_dir, &id, &author(), message)?;
    let rel = rel_to_root(&ctx.root, &path);
    Ok(json!({ "id": id.as_str(), "path": rel.as_str() }))
}

fn dep_add(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let id = parse_id(req_str(args, "id")?)?;
    let dep = parse_id(req_str(args, "dep_id")?)?;

    // Validation pipeline (DESIGN §5.4), mirroring `clove dep add`.
    if !ctx.store.exists(&id) {
        return Err(ToolError::Clove(CloveError::NotFound { id: id.to_string() }));
    }
    if !ctx.store.exists(&dep) {
        return Err(ToolError::Clove(CloveError::NotFound {
            id: dep.to_string(),
        }));
    }
    if id == dep {
        return Err(ToolError::Clove(CloveError::SelfDependency { id: id.to_string() }));
    }
    let (frontmatters, _errors) = ctx.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    if graph.check_would_cycle(&id, &dep) {
        return Err(ToolError::Clove(CloveError::DependencyCycle {
            from: id.to_string(),
            to: dep.to_string(),
            cycle: vec![id.to_string(), dep.to_string()],
        }));
    }

    let mut item = ctx.store.get(&id)?;
    if item.frontmatter.deps.contains(&dep) {
        return Err(ToolError::Clove(CloveError::DependencyExists {
            from: id.to_string(),
            to: dep.to_string(),
        }));
    }
    item.frontmatter.deps.push(dep);
    item.frontmatter.deps.sort();
    item.frontmatter.deps.dedup();
    let saved = ctx.store.update(&item, now_seconds())?;
    Ok(Value::Object(item_object(&saved)))
}

fn edit(ctx: &Ctx, args: &Value) -> Result<Value, ToolError> {
    let id = parse_id(req_str(args, "id")?)?;

    // Translate structured arguments into the `KEY=VALUE` tokens the shared
    // `apply_assignments` understands (one atomic multi-field write).
    let mut tokens: Vec<String> = Vec::new();
    if let Some(s) = arg_str(args, "status") {
        tokens.push(format!("status={s}"));
    }
    if let Some(p) = arg_u64(args, "priority") {
        tokens.push(format!("priority={p}"));
    }
    if let Some(t) = arg_str(args, "type") {
        tokens.push(format!("type={t}"));
    }
    if let Some(t) = arg_str(args, "title") {
        tokens.push(format!("title={t}"));
    }
    if let Some(v) = args.get("assignee") {
        tokens.push(format!("assignee={}", v.as_str().unwrap_or("")));
    }
    for l in str_array(args, "add_labels") {
        tokens.push(format!("labels+={l}"));
    }
    for l in str_array(args, "remove_labels") {
        tokens.push(format!("labels-={l}"));
    }
    if tokens.is_empty() {
        return Err(ToolError::Args("no fields to edit".to_owned()));
    }

    let mut item = ctx.store.get(&id)?;
    apply_assignments(&mut item.frontmatter, &tokens)?;
    let saved = ctx.store.update(&item, now_seconds())?;
    Ok(Value::Object(item_object(&saved)))
}

// ---- Shared helpers -----------------------------------------------------------

/// The comment author: `CLOVE_AUTHOR`, then `GIT_AUTHOR_EMAIL`, else `unknown`
/// (matches `clove comment`).
fn author() -> String {
    std::env::var("CLOVE_AUTHOR")
        .ok()
        .or_else(|| std::env::var("GIT_AUTHOR_EMAIL").ok())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn parse_filters(args: &Value) -> Result<Filters, ToolError> {
    Filters::parse(
        arg_str(args, "status"),
        arg_str(args, "type"),
        arg_str(args, "label"),
        arg_str(args, "assignee"),
        arg_u64(args, "priority").map(|n| n as u8),
    )
    .map_err(ToolError::Clove)
}

fn rank_of(ranks: &HashMap<CloveId, usize>, id: &CloveId) -> usize {
    ranks.get(id).copied().unwrap_or(usize::MAX)
}

/// Apply offset/limit and wrap a list of item objects into the standard payload.
fn page(objects: Vec<Map<String, Value>>, offset: usize, limit: Option<usize>) -> Value {
    let total = objects.len();
    let items: Vec<Value> = objects
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .map(Value::Object)
        .collect();
    json!({ "total": total, "returned": items.len(), "offset": offset, "items": items })
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

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    arg_str(args, key).ok_or_else(|| ToolError::Args(format!("missing required argument `{key}`")))
}

fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn arg_usize(args: &Value, key: &str) -> Option<usize> {
    arg_u64(args, key).map(|n| n as usize)
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `--limit` semantics: absent → `default`; `0` → unlimited; `n` → `n`.
fn arg_limit(args: &Value, default: usize) -> Option<usize> {
    match arg_u64(args, "limit") {
        None => Some(default),
        Some(0) => None,
        Some(n) => Some(n as usize),
    }
}

fn str_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

// ---- Schema builders ----------------------------------------------------------

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

/// Shallow-merge object `extra` into object `base`.
fn merge(mut base: Value, extra: Value) -> Value {
    if let (Value::Object(b), Value::Object(e)) = (&mut base, extra) {
        for (k, v) in e {
            b.insert(k, v);
        }
    }
    base
}

fn filter_props() -> Value {
    json!({
        "status": status_prop(),
        "type": type_prop(),
        "label": { "type": "string", "description": "Filter by a single label (case-insensitive)." },
        "assignee": { "type": "string", "description": "Filter by assignee." },
        "priority": priority_prop(),
        "limit": limit_prop(),
    })
}

fn id_prop() -> Value {
    json!({ "type": "string", "description": "The item id (e.g. `proj-7af3q2k9`)." })
}

fn status_prop() -> Value {
    json!({ "type": "string", "enum": ["open", "in_progress", "closed"], "description": "Item status." })
}

fn type_prop() -> Value {
    json!({ "type": "string", "enum": ["bug", "feature", "chore", "docs", "epic"], "description": "Item type." })
}

fn priority_prop() -> Value {
    json!({ "type": "integer", "minimum": 0, "maximum": 4, "description": "Priority (0 = highest, default 2)." })
}

fn limit_prop() -> Value {
    json!({ "type": "integer", "minimum": 0, "description": "Max results (0 = no limit)." })
}

fn str_array_prop(description: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": description })
}
