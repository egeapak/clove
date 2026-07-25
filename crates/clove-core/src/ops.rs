//! High-level item operations shared by the CLI mutation commands, the daemon
//! (which serializes writes through one process for topology B), and the MCP
//! server's direct fallback.
//!
//! Each high-level op performs the store I/O and returns the §7.4 item JSON (or
//! a small result object), so every surface produces byte-identical shapes. The
//! pure frontmatter mutators ([`set_status`], [`apply_assignments`]) are also
//! reused by the CLI's interactive edit path.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::store::ScanError;
use crate::view::item_object;
use crate::{
    add_comment, fields, list_comments, CloveError, CloveId, EditRequest, GraphStore, Item,
    ItemFrontmatter, ItemStatus, ItemStore, ItemType, NewItem,
};

/// Scan the store's frontmatter, refusing to proceed if *any* file failed to
/// parse. Store-wide validations (cycle detection, ancestry walks) that run
/// against a partial graph would silently admit invalid edges — a real cycle or
/// a hidden dependent living in the unparseable file — so a mutation that
/// depends on such a validation must fail loudly instead (`clove doctor` lists
/// the broken files).
fn scan_or_fail(store: &ItemStore) -> Result<Vec<ItemFrontmatter>, CloveError> {
    let (frontmatters, errors) = store.scan_frontmatter()?;
    if let Some(ScanError::ParseFailed { path, source }) = errors.first() {
        return Err(CloveError::ScanFailed {
            path: path.clone(),
            count: errors.len(),
            message: source.to_string(),
        });
    }
    Ok(frontmatters)
}

// The request types and the pure frontmatter mutators live in `clove-types`;
// re-export them here so the `clove_core::ops::*` paths used by the daemon, CLI,
// and MCP keep resolving.
pub use clove_types::{apply_assignments, set_status, NewSpec};

// ---- High-level operations (store I/O → JSON) --------------------------------

/// Create an item from a raw [`NewSpec`]. Returns `{ id, path }` (path relative
/// to the repo root), matching `clove new`.
pub fn create(
    store: &ItemStore,
    prefix: &str,
    default_type: ItemType,
    spec: NewSpec,
    now: DateTime<Utc>,
) -> Result<Value, CloveError> {
    let item_type = match spec.item_type.as_deref() {
        Some(t) => fields::parse_type(t)?,
        None => default_type,
    };
    let priority = match spec.priority {
        Some(p) => fields::parse_priority(p)?,
        None => crate::Priority::DEFAULT,
    };
    let labels = fields::parse_labels(&spec.labels)?;
    let deps = fields::parse_ids(&spec.deps)?;
    let parent = match spec.parent.as_deref() {
        Some(p) => Some(CloveId::new(p)?),
        None => None,
    };

    let new_item = NewItem {
        title: spec.title,
        item_type,
        priority,
        labels,
        deps,
        parent,
        assignee: spec.assignee,
        body: spec.body.unwrap_or_default(),
    };
    let item = store.create(prefix, new_item, now)?;
    let id = item.frontmatter.id.clone();
    let rel = rel_path(store, &id);
    Ok(json!({ "id": id.as_str(), "path": rel }))
}

/// Transition an item's status; returns the updated item object.
pub fn transition(
    store: &ItemStore,
    id: &CloveId,
    status: ItemStatus,
    now: DateTime<Utc>,
) -> Result<Value, CloveError> {
    let saved = store.update_with(id, now, |item| {
        set_status(&mut item.frontmatter, status, now);
        Ok(())
    })?;
    Ok(Value::Object(item_object(&saved)))
}

/// Apply `KEY=VALUE` edits atomically; returns the updated item object. Thin
/// shim over the unified [`crate::edit::apply_edit`] path.
pub fn edit(
    store: &ItemStore,
    id: &CloveId,
    assignments: &[String],
    now: DateTime<Utc>,
) -> Result<Value, CloveError> {
    let req = EditRequest::from_tokens(assignments)?;
    crate::edit::apply_edit(store, id, &req, now)
}

/// Append a comment; returns `{ id, path }` (path relative to the repo root).
pub fn comment(
    store: &ItemStore,
    id: &CloveId,
    author: &str,
    body: &str,
) -> Result<Value, CloveError> {
    if !store.exists(id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let path = add_comment(store.issues_dir(), id, author, body)?;
    let rel = path
        .strip_prefix(store.repo_root())
        .map(|p| p.to_string())
        .unwrap_or_else(|_| path.to_string());
    Ok(json!({ "id": id.as_str(), "path": rel }))
}

/// Add a hard dependency `id → dep_id` with the full validation pipeline
/// (DESIGN §5.4): existence, self-loop, cycle, duplicate. Returns the updated
/// item object.
pub fn dep_add(
    store: &ItemStore,
    id: &CloveId,
    dep_id: &CloveId,
    now: DateTime<Utc>,
) -> Result<Value, CloveError> {
    if id == dep_id {
        return Err(CloveError::SelfDependency { id: id.to_string() });
    }
    // Cheap up-front existence check for a clean error; re-validated under the
    // lock below.
    if !store.exists(dep_id) {
        return Err(CloveError::NotFound {
            id: dep_id.to_string(),
        });
    }
    // The cycle check and the write run under the same store-wide lock, so two
    // concurrent `dep add`s can't each pass a check against the pre-edit graph
    // and then both write, persisting a cycle (TOCTOU). `update_with` reads `id`
    // (→ NotFound if it's gone) under the lock too.
    let saved = store.update_with(id, now, |item| {
        if !store.exists(dep_id) {
            return Err(CloveError::NotFound {
                id: dep_id.to_string(),
            });
        }
        let frontmatters = scan_or_fail(store)?;
        let (graph, _dangling) = GraphStore::build(&frontmatters);
        if graph.check_would_cycle(id, dep_id) {
            return Err(CloveError::DependencyCycle {
                from: id.to_string(),
                to: dep_id.to_string(),
                cycle: vec![id.to_string(), dep_id.to_string()],
            });
        }
        if item.frontmatter.deps.contains(dep_id) {
            return Err(CloveError::DependencyExists {
                from: id.to_string(),
                to: dep_id.to_string(),
            });
        }
        item.frontmatter.deps.push(dep_id.clone());
        item.frontmatter.deps.sort();
        item.frontmatter.deps.dedup();
        Ok(())
    })?;
    Ok(Value::Object(item_object(&saved)))
}

/// Remove a hard dependency `id → dep_id`. Errors if `id` is unknown or does not
/// currently depend on `dep_id`. Returns the updated item object.
pub fn dep_remove(
    store: &ItemStore,
    id: &CloveId,
    dep_id: &CloveId,
    now: DateTime<Utc>,
) -> Result<Value, CloveError> {
    let saved = store.update_with(id, now, |item| {
        if !item.frontmatter.deps.contains(dep_id) {
            return Err(CloveError::InvalidField {
                field: "deps".to_owned(),
                reason: format!("{id} does not depend on {dep_id}"),
            });
        }
        item.frontmatter.deps.retain(|d| d != dep_id);
        Ok(())
    })?;
    Ok(Value::Object(item_object(&saved)))
}

/// Set (or, with `parent = None`, clear) an item's parent. Validates that the
/// parent exists and that the assignment does not create a parent cycle (the new
/// parent must not be `id` itself or any descendant of `id`). Returns the updated
/// item object.
pub fn set_parent(
    store: &ItemStore,
    id: &CloveId,
    parent: Option<&CloveId>,
    now: DateTime<Utc>,
) -> Result<Value, CloveError> {
    // The ancestry check and the write run under the same store-wide lock, so a
    // concurrent reparent can't invalidate the check between here and the write.
    let saved = store.update_with(id, now, |item| {
        match parent {
            None => item.frontmatter.parent = None,
            Some(parent) => {
                if parent == id {
                    return Err(CloveError::InvalidField {
                        field: "parent".to_owned(),
                        reason: format!("{id} cannot be its own parent"),
                    });
                }
                if !store.exists(parent) {
                    return Err(CloveError::NotFound {
                        id: parent.to_string(),
                    });
                }
                // Walk the proposed parent's ancestry; if we reach `id`, the
                // assignment would close a parent cycle. The `visited` set bounds
                // the walk so a *pre-existing* parent cycle in the store (a
                // representable but invalid state, e.g. from a bad hand-edit or
                // merge) can't hang us.
                let frontmatters = scan_or_fail(store)?;
                let parent_of: std::collections::HashMap<&CloveId, Option<&CloveId>> = frontmatters
                    .iter()
                    .map(|fm| (&fm.id, fm.parent.as_ref()))
                    .collect();
                let mut visited = std::collections::HashSet::new();
                let mut cursor = Some(parent);
                while let Some(node) = cursor {
                    if node == id {
                        return Err(CloveError::InvalidField {
                            field: "parent".to_owned(),
                            reason: format!(
                                "setting parent of {id} to {parent} would create a cycle"
                            ),
                        });
                    }
                    if !visited.insert(node) {
                        break; // a pre-existing cycle that doesn't involve `id`
                    }
                    cursor = parent_of.get(node).copied().flatten();
                }
                item.frontmatter.parent = Some(parent.clone());
            }
        }
        Ok(())
    })?;
    Ok(Value::Object(item_object(&saved)))
}

/// How many items a single item's hard-dependency closure may reach before
/// [`local_graph_terms`] gives up and the caller falls back to a whole-store
/// graph build. Bounds the pathological case (one long chain) while covering
/// every realistic dependency neighbourhood.
const LOCAL_CLOSURE_BUDGET: usize = 512;

/// The closure of `id` was too large to walk item-by-item; use the whole-store
/// graph instead.
struct BudgetExceeded;

/// `ready` / `blocked_by` for **one** item, without scanning the whole store.
///
/// Every term in those two answers is a query rooted at the item, not a global
/// one (see `GraphStore::{ready_items, blocked_items}`):
///
/// - unclosed and dangling hard deps — the item's own `deps`, one level;
/// - a malformed parent — a walk up the parent chain, which is functional (one
///   parent each), so an SCC through the item is exactly a cycle back to it;
/// - a hard-dependency cycle — whether the item is reachable from itself, i.e.
///   a DFS over its forward `deps` closure.
///
/// An item with no `deps` and no `parent` — the common case — needs no extra
/// reads at all. Returns `Err(BudgetExceeded)` rather than an approximation when
/// the closure is too large, so the result is always exactly what `GraphStore`
/// would have said.
fn local_graph_terms(
    store: &ItemStore,
    fm: &ItemFrontmatter,
) -> Result<(bool, Vec<String>), BudgetExceeded> {
    // Resolve an id the way the store's own scan does, or not at all. The scan
    // skips symlinks and non-regular files (DESIGN §12.3), so such a file is not
    // a graph node and its id must read as dangling here too — reaching it by
    // path would follow the symlink and disagree with `ready`/`blocked`.
    let read = |id: &CloveId| -> Option<ItemFrontmatter> {
        let path = store.path_for(id);
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() {
            return None;
        }
        crate::parse_frontmatter_file(&path).ok()
    };

    // Direct hard deps, split into dangling (no such item) and unclosed. Order
    // matches `GraphStore`: `blocking_deps` sorted, then `dangling_deps` in the
    // file's own `deps` order.
    let mut dangling: Vec<CloveId> = Vec::new();
    let mut open: Vec<CloveId> = Vec::new();
    for dep in &fm.deps {
        match read(dep) {
            None => dangling.push(dep.clone()),
            Some(target) if target.status != ItemStatus::Closed => open.push(dep.clone()),
            Some(_) => {}
        }
    }
    // Sorted but NOT deduped, matching `GraphStore::open_hard_dep_targets`: a
    // `deps` list naming the same id twice builds parallel edges, so the graph
    // reports it twice. `dep_add` prevents duplicates, but a hand-edit, an
    // import, or a merge can still produce them, and `clove_show` disagreeing
    // with `clove_blocked` on the same repo would be worse than the duplicate.
    open.sort();

    // Malformed parent: self-parent, or a parent chain that cycles back to us.
    // `visited` bounds a pre-existing cycle that does *not* contain this item.
    let mut malformed_parent = false;
    if let Some(parent) = &fm.parent {
        if parent == &fm.id {
            malformed_parent = true;
        } else {
            let mut visited: std::collections::HashSet<CloveId> =
                std::collections::HashSet::from([fm.id.clone()]);
            let mut cursor = Some(parent.clone());
            while let Some(node) = cursor {
                if node == fm.id {
                    malformed_parent = true;
                    break;
                }
                if !visited.insert(node.clone()) {
                    break;
                }
                if visited.len() > LOCAL_CLOSURE_BUDGET {
                    return Err(BudgetExceeded);
                }
                cursor = read(&node).and_then(|p| p.parent);
            }
        }
    }

    // Hard-dependency cycle: is this item reachable from itself? An item with no
    // deps has no outgoing edges and so cannot be, which skips the walk for the
    // overwhelmingly common case.
    let mut in_cycle = false;
    if !fm.deps.is_empty() {
        let mut seen: std::collections::HashSet<CloveId> = std::collections::HashSet::new();
        let mut stack: Vec<CloveId> = fm.deps.clone();
        while let Some(node) = stack.pop() {
            if node == fm.id {
                in_cycle = true;
                break;
            }
            if !seen.insert(node.clone()) {
                continue;
            }
            if seen.len() > LOCAL_CLOSURE_BUDGET {
                return Err(BudgetExceeded);
            }
            if let Some(next) = read(&node) {
                stack.extend(next.deps);
            }
        }
    }

    let excluded = in_cycle || malformed_parent;
    let active = fm.status.is_active();
    let ready = active && dangling.is_empty() && !excluded && open.is_empty();
    let blocked_by = if active && !excluded && !(open.is_empty() && dangling.is_empty()) {
        open.iter()
            .chain(dangling.iter())
            .map(CloveId::to_string)
            .collect()
    } else {
        Vec::new()
    };
    Ok((ready, blocked_by))
}

/// The computed `(ready, blocked_by)` pair for one item.
///
/// Prefers the item-local computation ([`local_graph_terms`]) and falls back to
/// a whole-store graph build only when the item's closure is too large, so the
/// answer is always exactly what `GraphStore` would say — see
/// `local_terms_match_the_graph_oracle`.
///
/// Shared by [`show`] and the CLI's `clove show`, which used to carry its own
/// copy of the whole-store version.
pub fn graph_terms(
    store: &ItemStore,
    fm: &ItemFrontmatter,
) -> Result<(bool, Vec<String>), CloveError> {
    if let Ok(terms) = local_graph_terms(store, fm) {
        return Ok(terms);
    }
    let (frontmatters, _errors) = store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ready = graph.ready_items().contains(&fm.id);
    let blocked_by: Vec<String> = graph
        .blocked_items()
        .into_iter()
        .find(|b| b.id == fm.id)
        .map(|b| {
            b.blocking_deps
                .iter()
                .chain(b.dangling_deps.iter())
                .map(CloveId::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok((ready, blocked_by))
}

/// The full §7.4 item object for `id`: frontmatter + body + comment_count +
/// computed `ready`/`blocked_by` (the same shape as `clove show --format json`).
pub fn show(store: &ItemStore, id: &CloveId) -> Result<Value, CloveError> {
    let item = store.get(id)?;
    let comment_count = list_comments(store.issues_dir(), id)
        .map(|c| c.len())
        .unwrap_or(0);

    let mut obj = item_object(&item);
    obj.insert("body".to_owned(), json!(item.body));
    obj.insert("comment_count".to_owned(), json!(comment_count));

    let (ready, blocked_by) = graph_terms(store, &item.frontmatter)?;
    obj.insert("ready".to_owned(), json!(ready));
    obj.insert("blocked_by".to_owned(), json!(blocked_by));
    Ok(Value::Object(obj))
}

/// An item's comment thread, oldest first, optionally capped to the most recent
/// `limit`. Returns the standard `{ total, returned, offset, items }` page, with
/// each element in the published `comment-list.json` element shape.
///
/// `limit` keeps the **newest** comments (the tail), not the first: a capped
/// thread is being sampled for what happened most recently. `skip_newest` then
/// walks backwards through history.
///
/// Deliberately *not* called `offset`: `ops::list`'s offset counts forward from
/// the start, and reusing the name for a window anchored at the opposite end
/// would let the usual paging idiom read history backwards without erroring.
pub fn comments(
    store: &ItemStore,
    id: &CloveId,
    window: crate::view::Page,
) -> Result<Value, CloveError> {
    if !store.exists(id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let all = list_comments(store.issues_dir(), id)?;
    let total = all.len();

    // Window from the newest end: `offset` skips the most recent (hence the
    // `skip_newest` spelling every surface exposes it under), `limit` caps how
    // many older ones follow. Saturating throughout so an over-large skip
    // yields an empty page rather than panicking.
    let end = total.saturating_sub(window.offset);
    let start = window.limit.map_or(0, |n| end.saturating_sub(n));
    let items: Vec<Value> = all[start..end]
        .iter()
        .map(|c| {
            json!({
                "author": c.author,
                "timestamp": c.timestamp.to_rfc3339(),
                "body": c.body,
            })
        })
        .collect();

    Ok(json!({
        "total": total,
        "returned": items.len(),
        "skip_newest": window.offset,
        "limit": window.reported_limit(),
        "items": items,
    }))
}

/// Compute the work-item analytics report (`clove stats`) from the file store
/// and return it as JSON. `top` caps the assignee/label breakdowns (0 = no cap).
pub fn stats(
    store: &ItemStore,
    top: usize,
    include_epics: bool,
    now: DateTime<Utc>,
) -> Result<Value, CloveError> {
    let (frontmatters, _errors) = store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let report = crate::stats::compute(
        &frontmatters,
        &graph,
        now,
        crate::StatsOptions { top, include_epics },
    );
    Ok(serde_json::to_value(&report).unwrap_or(Value::Null))
}

// ---- Read-list operations (file-based; always correct) -----------------------

/// List items matching `filters`, ordered by `(priority, topo, id)`, paginated.
/// Returns `{ total, returned, offset, items: [full objects] }`.
pub fn list(
    store: &ItemStore,
    filters: &crate::Filters,
    window: crate::view::Page,
) -> Result<Value, CloveError> {
    let (frontmatters, _errors) = store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ranks = graph.topological_ranks();
    let mut matched: Vec<ItemFrontmatter> = frontmatters
        .into_iter()
        .filter(|fm| filters.matches(fm))
        .collect();
    crate::view::sort_by_rank(&mut matched, &ranks);
    let objects: Vec<Value> = matched
        .iter()
        .map(|fm| Value::Object(crate::view::frontmatter_object(fm)))
        .collect();
    Ok(page(objects, window))
}

/// Items ready to work on now (open/in_progress, all hard deps closed, no
/// dangling), ordered `(priority, topo, id)`, filtered + paginated.
pub fn ready(
    store: &ItemStore,
    filters: &crate::Filters,
    window: crate::view::Page,
) -> Result<Value, CloveError> {
    let (frontmatters, _errors) = store.scan_frontmatter()?;
    let by_id: std::collections::HashMap<CloveId, ItemFrontmatter> = frontmatters
        .iter()
        .cloned()
        .map(|fm| (fm.id.clone(), fm))
        .collect();
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let objects: Vec<Value> = graph
        .ready_items()
        .iter()
        .filter_map(|id| by_id.get(id))
        .filter(|fm| filters.matches(fm))
        .map(|fm| Value::Object(crate::view::frontmatter_object(fm)))
        .collect();
    Ok(page(objects, window))
}

/// Items blocked by open or (with `include_warnings`) missing deps, each with a
/// `blocked_by` list, ordered `(priority, topo, id)`, filtered + paginated.
pub fn blocked(
    store: &ItemStore,
    filters: &crate::Filters,
    window: crate::view::Page,
) -> Result<Value, CloveError> {
    let (frontmatters, _errors) = store.scan_frontmatter()?;
    let by_id: std::collections::HashMap<CloveId, ItemFrontmatter> = frontmatters
        .iter()
        .cloned()
        .map(|fm| (fm.id.clone(), fm))
        .collect();
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ranks = graph.topological_ranks();

    let mut rows: Vec<(ItemFrontmatter, Vec<String>)> = graph
        .blocked_items()
        .into_iter()
        // Dangling-only items are included, per DESIGN §5.3: `GraphStore` puts
        // them in the blocked partition, and filtering them out here made them
        // invisible in *both* `ready` and `blocked` — a broken reference is a
        // data problem you need to see, not one to hide.
        .filter_map(|b| {
            by_id.get(&b.id).cloned().map(|fm| {
                let blocked_by: Vec<String> = b
                    .blocking_deps
                    .iter()
                    .chain(b.dangling_deps.iter())
                    .map(CloveId::to_string)
                    .collect();
                (fm, blocked_by)
            })
        })
        .collect();
    rows.retain(|(fm, _)| filters.matches(fm));
    rows.sort_by(|a, b| {
        a.0.priority
            .cmp(&b.0.priority)
            .then_with(|| {
                crate::view::rank_of(&ranks, &a.0.id).cmp(&crate::view::rank_of(&ranks, &b.0.id))
            })
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
    let objects: Vec<Value> = rows
        .into_iter()
        .map(|(fm, blocked_by)| {
            let mut obj = crate::view::frontmatter_object(&fm);
            obj.insert("blocked_by".to_owned(), json!(blocked_by));
            Value::Object(obj)
        })
        .collect();
    Ok(page(objects, window))
}

/// Case-insensitive substring search over title/labels/body; title matches rank
/// first, then labels, then body. Returns full item objects, paginated.
pub fn search(
    store: &ItemStore,
    text: &str,
    window: crate::view::Page,
) -> Result<Value, CloveError> {
    let needle = text.to_lowercase();
    let items = store.list()?;
    let mut hits: Vec<SearchHit> = Vec::new();
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
            hits.push((
                rank,
                fm.priority,
                fm.id.clone(),
                Value::Object(item_object(item)),
            ));
        }
    }
    sort_hits(&mut hits);
    let objects: Vec<Value> = hits.into_iter().map(|(_, _, _, o)| o).collect();
    Ok(page(objects, window))
}

/// One search hit: `(match class, priority, id, item JSON)`.
type SearchHit = (u8, crate::Priority, CloveId, Value);

/// Order search hits by `(match class, priority, id)` — a *total* order.
///
/// Ranking by match class alone left ties in `store.list()` order, which is raw
/// `read_dir` order: undefined, and it reshuffles when a file is added. That was
/// survivable while search had no `offset`, but paging over an unstable order
/// silently repeats and skips rows between requests. Split out so the ordering
/// can be tested against a deliberately scrambled input, which a test going
/// through the store cannot do — it would be at the mercy of `read_dir`.
fn sort_hits(hits: &mut [SearchHit]) {
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
}

/// The dependency tree rooted at `id` to `depth`, as a nested JSON object.
pub fn dep_tree(store: &ItemStore, id: &CloveId, depth: usize) -> Result<Value, CloveError> {
    if !store.exists(id) {
        return Err(CloveError::NotFound { id: id.to_string() });
    }
    let (frontmatters, _errors) = store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let root = graph
        .dep_tree(id, depth)
        .ok_or_else(|| CloveError::NotFound { id: id.to_string() })?;
    Ok(tree_to_json(&root))
}

fn tree_to_json(node: &crate::DepTreeNode) -> Value {
    json!({
        "id": node.id.as_str(),
        "title": node.title,
        "status": node.status.as_str(),
        "ready": node.ready,
        "cycle_ref": node.cycle_ref,
        "repeat_ref": node.repeat_ref,
        "children": node.children.iter().map(tree_to_json).collect::<Vec<_>>(),
    })
}

/// Apply the window and wrap a list of item values into the standard payload.
///
/// `limit` is echoed back so a caller can see the effective page size without
/// knowing the surface's default (`0` = unlimited, the same encoding it passes
/// in).
fn page(objects: Vec<Value>, window: crate::view::Page) -> Value {
    let (items, total) = window.apply(objects);
    json!({
        "total": total,
        "returned": items.len(),
        "offset": window.offset,
        "limit": window.reported_limit(),
        "items": items,
    })
}

/// The item's relative path under the repo root (best effort).
fn rel_path(store: &ItemStore, id: &CloveId) -> String {
    let path = store.path_for(id);
    path.strip_prefix(store.repo_root())
        .map(|p| p.to_string())
        .unwrap_or_else(|_| path.to_string())
}

/// Re-read `id` and return it (used by callers that want the [`Item`] rather
/// than its JSON, e.g. to render a human summary after a mutation).
pub fn reload(store: &ItemStore, id: &CloveId) -> Result<Item, CloveError> {
    store.get(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Page;
    use tempfile::TempDir;

    fn store() -> (TempDir, ItemStore) {
        let dir = TempDir::new().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join(".clove/issues")).unwrap();
        let store = ItemStore::new(root);
        (dir, store)
    }

    fn new_id(store: &ItemStore, title: &str) -> CloveId {
        let v = create(
            store,
            "proj",
            ItemType::Feature,
            NewSpec {
                title: title.to_owned(),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        CloveId::new(v["id"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn create_then_show_round_trips() {
        let (_d, store) = store();
        let v = create(
            &store,
            "proj",
            ItemType::Bug,
            NewSpec {
                title: "fix it".to_owned(),
                priority: Some(1),
                labels: vec!["Area:Core".to_owned()],
                body: Some("# Title\n\nbody".to_owned()),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        let id = CloveId::new(v["id"].as_str().unwrap()).unwrap();

        let shown = show(&store, &id).unwrap();
        assert_eq!(shown["title"], "fix it");
        assert_eq!(shown["type"], "bug");
        assert_eq!(shown["priority"], 1);
        // Label was canonicalized on the way in.
        assert_eq!(shown["labels"], json!(["area:core"]));
        assert_eq!(shown["body"], "# Title\n\nbody");
        assert_eq!(shown["comment_count"], 0);
        assert_eq!(shown["ready"], true);
        assert_eq!(shown["blocked_by"], json!([]));
    }

    #[test]
    fn create_rejects_bad_fields() {
        let (_d, store) = store();
        // Negative: invalid type, out-of-range priority, malformed dep id.
        assert!(create(
            &store,
            "proj",
            ItemType::Feature,
            NewSpec {
                title: "x".to_owned(),
                item_type: Some("saga".to_owned()),
                ..Default::default()
            },
            Utc::now()
        )
        .is_err());
        assert!(create(
            &store,
            "proj",
            ItemType::Feature,
            NewSpec {
                title: "x".to_owned(),
                priority: Some(9),
                ..Default::default()
            },
            Utc::now()
        )
        .is_err());
        assert!(create(
            &store,
            "proj",
            ItemType::Feature,
            NewSpec {
                title: "x".to_owned(),
                deps: vec!["not a real id".to_owned()],
                ..Default::default()
            },
            Utc::now()
        )
        .is_err());
    }

    #[test]
    fn transition_sets_and_clears_closed() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        let closed = transition(&store, &id, ItemStatus::Closed, Utc::now()).unwrap();
        assert_eq!(closed["status"], "closed");
        assert!(closed["closed"].is_string(), "closed timestamp set");
        let reopened = transition(&store, &id, ItemStatus::Open, Utc::now()).unwrap();
        assert_eq!(reopened["status"], "open");
        assert!(reopened["closed"].is_null(), "closed cleared on reopen");
    }

    #[test]
    fn edit_applies_multiple_fields_and_labels() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        let v = edit(
            &store,
            &id,
            &[
                "priority=0".to_owned(),
                "assignee=alice".to_owned(),
                "labels+=urgent".to_owned(),
            ],
            Utc::now(),
        )
        .unwrap();
        assert_eq!(v["priority"], 0);
        assert_eq!(v["assignee"], "alice");
        assert_eq!(v["labels"], json!(["urgent"]));
        // Negative: an unknown field is rejected.
        assert!(edit(&store, &id, &["bogus=1".to_owned()], Utc::now()).is_err());
    }

    #[test]
    fn dep_add_validation_pipeline() {
        let (_d, store) = store();
        let a = new_id(&store, "a");
        let b = new_id(&store, "b");

        // Positive: a depends on b.
        let v = dep_add(&store, &a, &b, Utc::now()).unwrap();
        assert_eq!(v["deps"], json!([b.as_str()]));

        // Negative: duplicate.
        assert!(matches!(
            dep_add(&store, &a, &b, Utc::now()),
            Err(CloveError::DependencyExists { .. })
        ));
        // Negative: self-loop.
        assert!(matches!(
            dep_add(&store, &a, &a, Utc::now()),
            Err(CloveError::SelfDependency { .. })
        ));
        // Negative: cycle (b → a, when a → b already).
        assert!(matches!(
            dep_add(&store, &b, &a, Utc::now()),
            Err(CloveError::DependencyCycle { .. })
        ));
        // Negative: missing dependency target.
        let missing = CloveId::new("proj-ZZZZZZZZ").unwrap();
        assert!(matches!(
            dep_add(&store, &a, &missing, Utc::now()),
            Err(CloveError::NotFound { .. })
        ));
    }

    #[test]
    fn create_enforces_edit_path_validations() {
        let (_d, store) = store();
        let existing = new_id(&store, "real");

        // Empty / whitespace title is rejected (matches the edit path).
        assert!(matches!(
            create(
                &store,
                "proj",
                ItemType::Feature,
                NewSpec {
                    title: "   ".to_owned(),
                    ..Default::default()
                },
                Utc::now()
            ),
            Err(CloveError::InvalidField { .. })
        ));
        // A blank assignee is rejected — "unassigned" is `None`, never `Some("")`.
        assert!(matches!(
            create(
                &store,
                "proj",
                ItemType::Feature,
                NewSpec {
                    title: "ok".to_owned(),
                    assignee: Some("  ".to_owned()),
                    ..Default::default()
                },
                Utc::now()
            ),
            Err(CloveError::InvalidField { .. })
        ));
        // A well-formed but non-existent dep id is a dangling ref (NotFound),
        // just like `dep add` to a missing target.
        assert!(matches!(
            create(
                &store,
                "proj",
                ItemType::Feature,
                NewSpec {
                    title: "ok".to_owned(),
                    deps: vec!["proj-ZZZZZZZZ".to_owned()],
                    ..Default::default()
                },
                Utc::now()
            ),
            Err(CloveError::NotFound { .. })
        ));
        // A dangling parent is likewise NotFound.
        assert!(matches!(
            create(
                &store,
                "proj",
                ItemType::Feature,
                NewSpec {
                    title: "ok".to_owned(),
                    parent: Some("proj-ZZZZZZZZ".to_owned()),
                    ..Default::default()
                },
                Utc::now()
            ),
            Err(CloveError::NotFound { .. })
        ));
        // Sanity: an existing dep/parent still creates fine.
        assert!(create(
            &store,
            "proj",
            ItemType::Feature,
            NewSpec {
                title: "ok".to_owned(),
                deps: vec![existing.to_string()],
                ..Default::default()
            },
            Utc::now()
        )
        .is_ok());
    }

    #[test]
    fn cycle_validation_refuses_a_partially_unparseable_store() {
        // If a file fails to parse, the graph built for the cycle/ancestry check
        // is incomplete; validating against it could admit a real cycle. Both
        // graph-edge ops must refuse rather than validate against a partial store.
        let (_d, store) = store();
        let a = new_id(&store, "a");
        let b = new_id(&store, "b");
        std::fs::write(
            store.issues_dir().join("proj-BROKEN01.md"),
            "---\nnot: [valid yaml\n---\nbody",
        )
        .unwrap();

        assert!(matches!(
            dep_add(&store, &a, &b, Utc::now()),
            Err(CloveError::ScanFailed { .. })
        ));
        assert!(matches!(
            set_parent(&store, &a, Some(&b), Utc::now()),
            Err(CloveError::ScanFailed { .. })
        ));
    }

    #[test]
    fn concurrent_dep_adds_do_not_lose_updates() {
        // Regression for the read-modify-write lock window: each `dep add` reads
        // the item, appends one dep, and writes. Without a store-wide lock held
        // across that whole window, concurrent writers overwrite each other and
        // deps are silently lost. With it, all N serialize and survive.
        let (_d, store) = store();
        let root = new_id(&store, "root");
        let deps: Vec<CloveId> = (0..8).map(|i| new_id(&store, &format!("d{i}"))).collect();

        let handles: Vec<_> = deps
            .iter()
            .map(|dep| {
                let store = store.clone();
                let root = root.clone();
                let dep = dep.clone();
                std::thread::spawn(move || dep_add(&store, &root, &dep, Utc::now()).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let reloaded = store.get(&root).unwrap();
        assert_eq!(
            reloaded.frontmatter.deps.len(),
            deps.len(),
            "every concurrent dep add must survive (no lost updates)"
        );
    }

    #[test]
    fn dep_remove_removes_and_errors_when_absent() {
        let (_d, store) = store();
        let a = new_id(&store, "a");
        let b = new_id(&store, "b");
        dep_add(&store, &a, &b, Utc::now()).unwrap();
        let v = dep_remove(&store, &a, &b, Utc::now()).unwrap();
        assert_eq!(v["deps"], json!([]));
        // Strict: removing a dependency that isn't present errors.
        assert!(matches!(
            dep_remove(&store, &a, &b, Utc::now()),
            Err(CloveError::InvalidField { .. })
        ));
    }

    #[test]
    fn set_parent_sets_clears_and_validates() {
        let (_d, store) = store();
        let a = new_id(&store, "a");
        let b = new_id(&store, "b");

        // Set, then clear.
        let v = set_parent(&store, &a, Some(&b), Utc::now()).unwrap();
        assert_eq!(v["parent"], b.as_str());
        let v = set_parent(&store, &a, None, Utc::now()).unwrap();
        assert!(v["parent"].is_null());

        // Self-parent is rejected.
        assert!(matches!(
            set_parent(&store, &a, Some(&a), Utc::now()),
            Err(CloveError::InvalidField { .. })
        ));
        // A missing parent is NotFound.
        let missing = CloveId::new("proj-ZZZZZZZZ").unwrap();
        assert!(matches!(
            set_parent(&store, &a, Some(&missing), Utc::now()),
            Err(CloveError::NotFound { .. })
        ));
        // A cycle (a's parent is b, so b's parent can't become a) is rejected.
        set_parent(&store, &a, Some(&b), Utc::now()).unwrap();
        assert!(matches!(
            set_parent(&store, &b, Some(&a), Utc::now()),
            Err(CloveError::InvalidField { .. })
        ));
    }

    #[test]
    fn set_parent_terminates_on_preexisting_cycle() {
        // A parent cycle is "representable but invalid"; set_parent must not hang
        // walking the ancestry of a parent whose chain already cycles.
        let (_d, store) = store();
        let b = new_id(&store, "b");
        let c = new_id(&store, "c");
        // Force b ↔ c directly (bypassing set_parent's own cycle guard).
        let mut ib = store.get(&b).unwrap();
        ib.frontmatter.parent = Some(c.clone());
        store.update(&ib, Utc::now()).unwrap();
        let mut ic = store.get(&c).unwrap();
        ic.frontmatter.parent = Some(b.clone());
        store.update(&ic, Utc::now()).unwrap();

        // Parenting a fresh item under b must terminate (the cycle excludes d).
        let d = new_id(&store, "d");
        let v = set_parent(&store, &d, Some(&b), Utc::now()).unwrap();
        assert_eq!(v["parent"], b.as_str());
    }

    #[test]
    fn apply_edit_empty_request_is_a_no_op() {
        let (_d, store) = store();
        let id = new_id(&store, "task");
        let before = store.get(&id).unwrap().frontmatter.updated;
        // An all-absent EditRequest must not rewrite the file / bump `updated`.
        let v = crate::apply_edit(
            &store,
            &id,
            &crate::EditRequest::default(),
            Utc::now() + chrono::Duration::seconds(5),
        )
        .unwrap();
        assert_eq!(v["id"], id.as_str());
        assert_eq!(store.get(&id).unwrap().frontmatter.updated, before);
    }

    #[test]
    fn list_ready_blocked_partition() {
        let (_d, store) = store();
        let a = new_id(&store, "a");
        let b = new_id(&store, "b");
        dep_add(&store, &a, &b, Utc::now()).unwrap(); // a blocked by open b

        let all = list(&store, &crate::Filters::default(), Page::unlimited()).unwrap();
        assert_eq!(all["total"], 2);
        assert_eq!(all["items"].as_array().unwrap().len(), 2);

        let ready_v = ready(&store, &crate::Filters::default(), Page::unlimited()).unwrap();
        let ready_ids: Vec<&str> = ready_v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap())
            .collect();
        assert_eq!(ready_ids, vec![b.as_str()], "only b is ready");

        let blocked_v = blocked(&store, &crate::Filters::default(), Page::unlimited()).unwrap();
        let items = blocked_v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], a.as_str());
        assert_eq!(items[0]["blocked_by"], json!([b.as_str()]));
    }

    #[test]
    fn list_filters_and_paginates() {
        let (_d, store) = store();
        for i in 0..5 {
            new_id(&store, &format!("t{i}"));
        }
        // Edge: limit caps the page but total reflects the full match count.
        let v = list(&store, &crate::Filters::default(), Page::new(0, Some(2), 0)).unwrap();
        assert_eq!(v["total"], 5);
        assert_eq!(v["returned"], 2);
        // Filter that matches nothing → empty page, total 0.
        let none = list(
            &store,
            &crate::Filters::parse(Some("closed"), None, None, None, None).unwrap(),
            Page::unlimited(),
        )
        .unwrap();
        assert_eq!(none["total"], 0);
        assert!(none["items"].as_array().unwrap().is_empty());
    }

    #[test]
    fn search_ranks_title_before_body() {
        let (_d, store) = store();
        create(
            &store,
            "proj",
            ItemType::Feature,
            NewSpec {
                title: "widget".to_owned(),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        create(
            &store,
            "proj",
            ItemType::Feature,
            NewSpec {
                title: "other".to_owned(),
                body: Some("mentions widget in body".to_owned()),
                ..Default::default()
            },
            Utc::now(),
        )
        .unwrap();
        let v = search(&store, "WIDGET", Page::unlimited()).unwrap();
        let titles: Vec<&str> = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, vec!["widget", "other"], "title hit ranks first");
        // Negative: a needle present nowhere returns nothing.
        assert_eq!(
            search(&store, "zzzzz", Page::unlimited()).unwrap()["total"],
            0
        );
    }

    /// Search results are totally ordered, so paging over them is stable.
    ///
    /// Driven directly against a scrambled hit list rather than through the
    /// store: `search` reads the issues directory, so a store-based test sees
    /// whatever order `read_dir` returns — which on this filesystem is already
    /// sorted, making such a test pass with the bug still in place.
    #[test]
    fn search_hits_are_totally_ordered() {
        let hit = |class: u8, priority: u8, raw: &str| -> SearchHit {
            (
                class,
                crate::Priority::new(priority).unwrap(),
                CloveId::new(raw).unwrap(),
                Value::Null,
            )
        };
        // Scrambled, and built so every key matters: three share a match class,
        // two of those share a priority.
        let mut hits = vec![
            hit(2, 0, "proj-BBBBBBBB"),
            hit(0, 3, "proj-CCCCCCCC"),
            hit(0, 1, "proj-ZZZZZZZZ"),
            hit(1, 4, "proj-AAAAAAAA"),
            hit(0, 3, "proj-AAAAAAAB"),
        ];
        sort_hits(&mut hits);
        let order: Vec<&str> = hits.iter().map(|h| h.2.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "proj-ZZZZZZZZ", // class 0, p1
                "proj-AAAAAAAB", // class 0, p3, id sorts first
                "proj-CCCCCCCC", // class 0, p3
                "proj-AAAAAAAA", // class 1
                "proj-BBBBBBBB", // class 2 — despite arriving first, at p0
            ],
            "class, then priority, then id — no input order survives"
        );

        // The result does not depend on the order the hits arrive in.
        let mut reversed: Vec<SearchHit> = hits.iter().rev().cloned().collect();
        sort_hits(&mut reversed);
        let reordered: Vec<&str> = reversed.iter().map(|h| h.2.as_str()).collect();
        assert_eq!(reordered, order, "a permuted input must sort identically");
    }

    /// End-to-end: consecutive windows over a search tile the result set exactly
    /// once, which is what `offset` promises a paging client.
    #[test]
    fn search_windows_tile_the_result_set() {
        let (_d, store) = store();
        for _ in 0..8 {
            create(
                &store,
                "proj",
                ItemType::Feature,
                NewSpec {
                    title: "widget".to_owned(),
                    ..Default::default()
                },
                Utc::now(),
            )
            .unwrap();
        }
        let ids = |p: Page| -> Vec<String> {
            search(&store, "widget", p).unwrap()["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o["id"].as_str().unwrap().to_owned())
                .collect()
        };
        let all = ids(Page::unlimited());
        assert_eq!(all.len(), 8);
        let paged: Vec<String> = (0..4)
            .flat_map(|i| ids(Page::new(i * 2, Some(2), 0)))
            .collect();
        assert_eq!(paged, all, "windows must tile without gaps or repeats");
    }

    #[test]
    fn dep_tree_renders_and_rejects_missing() {
        let (_d, store) = store();
        let a = new_id(&store, "a");
        let b = new_id(&store, "b");
        dep_add(&store, &a, &b, Utc::now()).unwrap();
        let tree = dep_tree(&store, &a, 5).unwrap();
        assert_eq!(tree["id"], a.as_str());
        assert_eq!(tree["children"][0]["id"], b.as_str());
        // Negative: unknown root.
        assert!(dep_tree(&store, &CloveId::new("proj-ZZZZZZZZ").unwrap(), 5).is_err());
    }

    #[test]
    fn stats_reports_totals() {
        let (_d, store) = store();
        new_id(&store, "a");
        let b = new_id(&store, "b");
        transition(&store, &b, ItemStatus::Closed, Utc::now()).unwrap();
        let v = stats(&store, 10, true, Utc::now()).unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["by_status"]["closed"], 1);
        assert_eq!(v["by_status"]["open"], 1);
    }

    /// `local_graph_terms` must agree with `GraphStore` for **every** item in a
    /// store, including the shapes the graph treats specially: hard-dep cycles
    /// (2-cycle and self-loop), malformed parents (self-parent and parent
    /// cycle), dangling deps, and a closed dep that still points back.
    ///
    /// This is the guard that lets `show` skip the whole-store build: a silent
    /// divergence here would be wrong, not merely slow.
    #[test]
    fn local_terms_match_the_graph_oracle() {
        let (_d, store) = store();

        // A plain ready item, and a simple blocked chain.
        let a = new_id(&store, "a");
        let b = new_id(&store, "b");
        dep_add(&store, &a, &b, Utc::now()).unwrap(); // a blocked by open b
                                                      // A closed dep: the dependent becomes ready again.
        let c = new_id(&store, "c");
        let d = new_id(&store, "d");
        dep_add(&store, &c, &d, Utc::now()).unwrap();
        transition(&store, &d, ItemStatus::Closed, Utc::now()).unwrap();
        // An inactive item.
        let closed = new_id(&store, "closed");
        transition(&store, &closed, ItemStatus::Closed, Utc::now()).unwrap();

        // Shapes `dep_add`/`set_parent` refuse to create: written directly.
        let raw = |id: &CloveId, mutate: &dyn Fn(&mut ItemFrontmatter)| {
            let mut item = store.get(id).unwrap();
            mutate(&mut item.frontmatter);
            store.update(&item, Utc::now()).unwrap();
        };
        // Dangling dep.
        let dangler = new_id(&store, "dangler");
        let ghost = CloveId::new("proj-ZZZZZZZZ").unwrap();
        raw(&dangler, &|fm| fm.deps = vec![ghost.clone()]);
        // Hard self-loop.
        let selfloop = new_id(&store, "selfloop");
        raw(&selfloop, &|fm| fm.deps = vec![fm.id.clone()]);
        // A 2-cycle.
        let cyc1 = new_id(&store, "cyc1");
        let cyc2 = new_id(&store, "cyc2");
        raw(&cyc1, &|fm| fm.deps = vec![cyc2.clone()]);
        raw(&cyc2, &|fm| fm.deps = vec![cyc1.clone()]);
        // Self-parent.
        let selfparent = new_id(&store, "selfparent");
        raw(&selfparent, &|fm| fm.parent = Some(fm.id.clone()));
        // A parent cycle between two items, plus a child hanging off it (which
        // is NOT itself in the cycle and must stay unexcluded).
        let p1 = new_id(&store, "p1");
        let p2 = new_id(&store, "p2");
        raw(&p1, &|fm| fm.parent = Some(p2.clone()));
        raw(&p2, &|fm| fm.parent = Some(p1.clone()));
        let child = new_id(&store, "child");
        raw(&child, &|fm| fm.parent = Some(p1.clone()));
        // An item depending on a cycle member (reaches the cycle but is not in it).
        let near = new_id(&store, "near");
        raw(&near, &|fm| fm.deps = vec![cyc1.clone()]);
        // The same dep listed twice. The frontmatter *writer* sorts and dedups
        // list fields, so this has to be patched into the file text directly —
        // going through `store.update` would silently drop the duplicate and
        // make this fixture vacuous. The graph builds parallel edges and reports
        // the id twice, so the local walk must not dedup it away either.
        let dup = new_id(&store, "dup");
        let dup_path = store.path_for(&dup);
        let text = std::fs::read_to_string(&dup_path).unwrap();
        std::fs::write(
            &dup_path,
            text.replace("deps: []", &format!("deps: [{b}, {b}]")),
        )
        .unwrap();

        // An item whose file is a symlink. The store's scan skips symlinks
        // (DESIGN §12.3), so it is not a graph node and a dependent sees a
        // dangling ref.
        //
        // Two details make this discriminate. The link target must carry the
        // *same* id as the link's stem, or `parse_frontmatter_file`'s id/stem
        // check rejects it and both paths agree by accident. And it must be
        // `closed`: resolving the link would then report "dependency satisfied"
        // (ready) where the graph reports "dependency missing" (blocked). An
        // open target would read as blocked either way and prove nothing.
        #[cfg(unix)]
        let via_link = {
            let linked = new_id(&store, "linked");
            let via_link = new_id(&store, "via-link");
            raw(&via_link, &|fm| fm.deps = vec![linked.clone()]);

            // A validly-closed item, re-stamped with `linked`'s id.
            let sink = new_id(&store, "sink");
            transition(&store, &sink, ItemStatus::Closed, Utc::now()).unwrap();
            let closed_text = std::fs::read_to_string(store.path_for(&sink))
                .unwrap()
                .replace(sink.as_str(), linked.as_str());
            // Outside `issues/`, so the scan cannot pick the target up directly.
            let target = store.repo_root().join("linked-elsewhere.md");
            std::fs::write(&target, closed_text).unwrap();

            let link_path = store.path_for(&linked);
            std::fs::remove_file(&link_path).unwrap();
            std::os::unix::fs::symlink(target.as_std_path(), link_path.as_std_path()).unwrap();
            via_link
        };

        // The oracle: one whole-store graph build.
        let (frontmatters, _errors) = store.scan_frontmatter().unwrap();
        let (graph, _dangling) = GraphStore::build(&frontmatters);
        let ready_set = graph.ready_items();
        let blocked = graph.blocked_items();

        for fm in &frontmatters {
            let want_ready = ready_set.contains(&fm.id);
            let want_blocked: Vec<String> = blocked
                .iter()
                .find(|b| b.id == fm.id)
                .map(|b| {
                    b.blocking_deps
                        .iter()
                        .chain(b.dangling_deps.iter())
                        .map(CloveId::to_string)
                        .collect()
                })
                .unwrap_or_default();

            let (got_ready, got_blocked) = match local_graph_terms(&store, fm) {
                Ok(terms) => terms,
                Err(BudgetExceeded) => panic!("{} exceeded the budget unexpectedly", fm.id),
            };
            assert_eq!(got_ready, want_ready, "ready mismatch for {}", fm.id);
            assert_eq!(
                got_blocked, want_blocked,
                "blocked_by mismatch for {}",
                fm.id
            );
        }

        // And the shapes above are actually present, so this is not vacuous.
        assert!(!ready_set.contains(&cyc1), "cycle member must be excluded");
        assert!(!ready_set.contains(&selfloop), "self-loop must be excluded");
        assert!(
            !ready_set.contains(&selfparent),
            "self-parent must be excluded"
        );
        assert!(
            !ready_set.contains(&p1),
            "parent-cycle member must be excluded"
        );
        assert!(
            ready_set.contains(&child),
            "a child of a cycle is not itself in it"
        );
        assert!(!ready_set.contains(&dangler), "a dangling dep blocks");
        // The duplicate really reached disk (the writer would have deduped it).
        let dup_fm = frontmatters.iter().find(|f| f.id == dup).unwrap();
        assert_eq!(dup_fm.deps.len(), 2, "duplicate dep fixture must be real");
        // The symlinked item is not a node, so its dependent is blocked, not
        // ready — the state that differs from following the link.
        #[cfg(unix)]
        assert!(
            !ready_set.contains(&via_link),
            "a dep whose file is a symlink must read as dangling, not resolved"
        );
    }

    /// A closure larger than the budget falls back rather than answering from a
    /// partial walk — and `show` still returns the oracle's answer.
    #[test]
    fn an_oversized_closure_falls_back_to_the_whole_store_graph() {
        let (_d, store) = store();
        let ids: Vec<CloveId> = (0..(LOCAL_CLOSURE_BUDGET + 8))
            .map(|i| new_id(&store, &format!("n{i}")))
            .collect();
        // One long chain: n0 -> n1 -> n2 -> ... so n0's closure is the whole store.
        for pair in ids.windows(2) {
            let mut item = store.get(&pair[0]).unwrap();
            item.frontmatter.deps = vec![pair[1].clone()];
            store.update(&item, Utc::now()).unwrap();
        }
        let head = store.get(&ids[0]).unwrap();
        assert!(
            local_graph_terms(&store, &head.frontmatter).is_err(),
            "a closure past the budget must decline rather than approximate"
        );
        // `show` still answers, via the fallback, and correctly: n0 waits on n1.
        let shown = show(&store, &ids[0]).unwrap();
        assert_eq!(shown["ready"], false);
        assert_eq!(shown["blocked_by"], json!([ids[1].as_str()]));
    }

    #[test]
    fn comments_page_from_the_newest_end() {
        let (_d, store) = store();
        let id = new_id(&store, "chatty");
        for i in 0..5 {
            comment(&store, &id, "me", &format!("note {i}")).unwrap();
        }

        // No limit: the whole thread, oldest first.
        let all = comments(&store, &id, Page::new(0, None, 0)).unwrap();
        assert_eq!(all["total"], 5);
        assert_eq!(all["returned"], 5);
        assert_eq!(all["items"][0]["body"], "note 0");

        // A limit keeps the NEWEST n, still in chronological order.
        let last2 = comments(&store, &id, Page::new(0, Some(2), 0)).unwrap();
        assert_eq!(last2["returned"], 2);
        assert_eq!(last2["items"][0]["body"], "note 3");
        assert_eq!(last2["items"][1]["body"], "note 4");
        assert_eq!(last2["total"], 5, "total is the unpaginated count");

        // `skip_newest` walks backwards through history from the newest end.
        let older = comments(&store, &id, Page::new(2, Some(2), 0)).unwrap();
        assert_eq!(older["items"][0]["body"], "note 1");
        assert_eq!(older["items"][1]["body"], "note 2");

        // Edge: skipping past the end is an empty page, not a panic.
        let past = comments(&store, &id, Page::new(99, Some(2), 0)).unwrap();
        assert_eq!(past["returned"], 0);
        assert_eq!(past["total"], 5);

        // An item with no comments is an empty page, not an error.
        let quiet = new_id(&store, "quiet");
        assert_eq!(
            comments(&store, &quiet, Page::new(0, None, 0)).unwrap()["total"],
            0
        );

        // A missing item is NotFound (matching `show`).
        assert!(matches!(
            comments(
                &store,
                &CloveId::new("proj-ZZZZZZZZ").unwrap(),
                Page::unlimited()
            ),
            Err(CloveError::NotFound { .. })
        ));
    }

    #[test]
    fn comment_round_trips_and_blocks_when_ready() {
        let (_d, store) = store();
        let a = new_id(&store, "a");
        let b = new_id(&store, "b");
        // a depends on open b → a is blocked, not ready.
        dep_add(&store, &a, &b, Utc::now()).unwrap();
        let shown = show(&store, &a).unwrap();
        assert_eq!(shown["ready"], false);
        assert_eq!(shown["blocked_by"], json!([b.as_str()]));

        // Comment on a missing item errors; on a real item it returns a path.
        assert!(comment(&store, &CloveId::new("proj-ZZZZZZZZ").unwrap(), "me", "hi").is_err());
        let c = comment(&store, &a, "me@example.com", "working on it").unwrap();
        assert_eq!(c["id"], a.as_str());
        assert!(c["path"].as_str().unwrap().contains(a.as_str()));
        assert_eq!(show(&store, &a).unwrap()["comment_count"], 1);
    }
}
