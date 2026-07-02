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

/// The full §7.4 item object for `id`: frontmatter + body + comment_count +
/// computed `ready`/`blocked_by` (a whole-store graph build, like `clove show
/// --format json`).
pub fn show(store: &ItemStore, id: &CloveId) -> Result<Value, CloveError> {
    let item = store.get(id)?;
    let comment_count = list_comments(store.issues_dir(), id)
        .map(|c| c.len())
        .unwrap_or(0);

    let mut obj = item_object(&item);
    obj.insert("body".to_owned(), json!(item.body));
    obj.insert("comment_count".to_owned(), json!(comment_count));

    let (frontmatters, _errors) = store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    let ready = graph.ready_items().contains(id);
    let blocked_by: Vec<String> = graph
        .blocked_items()
        .into_iter()
        .find(|b| &b.id == id)
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
    offset: usize,
    limit: Option<usize>,
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
    Ok(page(objects, offset, limit))
}

/// Items ready to work on now (open/in_progress, all hard deps closed, no
/// dangling), ordered `(priority, topo, id)`, filtered + paginated.
pub fn ready(
    store: &ItemStore,
    filters: &crate::Filters,
    limit: Option<usize>,
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
    Ok(page(objects, 0, limit))
}

/// Items blocked by open or (with `include_warnings`) missing deps, each with a
/// `blocked_by` list, ordered `(priority, topo, id)`, filtered + paginated.
pub fn blocked(
    store: &ItemStore,
    filters: &crate::Filters,
    include_warnings: bool,
    limit: Option<usize>,
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
        .filter(|b| include_warnings || !b.blocking_deps.is_empty())
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
    Ok(page(objects, 0, limit))
}

/// Case-insensitive substring search over title/labels/body; title matches rank
/// first, then labels, then body. Returns full item objects, paginated.
pub fn search(store: &ItemStore, text: &str, limit: Option<usize>) -> Result<Value, CloveError> {
    let needle = text.to_lowercase();
    let items = store.list()?;
    let mut hits: Vec<(u8, Value)> = Vec::new();
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
            hits.push((rank, Value::Object(item_object(item))));
        }
    }
    hits.sort_by_key(|a| a.0);
    let objects: Vec<Value> = hits.into_iter().map(|(_, o)| o).collect();
    Ok(page(objects, 0, limit))
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
        "children": node.children.iter().map(tree_to_json).collect::<Vec<_>>(),
    })
}

/// Apply offset/limit and wrap a list of item values into the standard payload.
fn page(objects: Vec<Value>, offset: usize, limit: Option<usize>) -> Value {
    let total = objects.len();
    let items: Vec<Value> = objects
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect();
    json!({ "total": total, "returned": items.len(), "offset": offset, "items": items })
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

        let all = list(&store, &crate::Filters::default(), 0, None).unwrap();
        assert_eq!(all["total"], 2);
        assert_eq!(all["items"].as_array().unwrap().len(), 2);

        let ready_v = ready(&store, &crate::Filters::default(), None).unwrap();
        let ready_ids: Vec<&str> = ready_v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap())
            .collect();
        assert_eq!(ready_ids, vec![b.as_str()], "only b is ready");

        let blocked_v = blocked(&store, &crate::Filters::default(), false, None).unwrap();
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
        let v = list(&store, &crate::Filters::default(), 0, Some(2)).unwrap();
        assert_eq!(v["total"], 5);
        assert_eq!(v["returned"], 2);
        // Filter that matches nothing → empty page, total 0.
        let none = list(
            &store,
            &crate::Filters::parse(Some("closed"), None, None, None, None).unwrap(),
            0,
            None,
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
        let v = search(&store, "WIDGET", None).unwrap();
        let titles: Vec<&str> = v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, vec!["widget", "other"], "title hit ranks first");
        // Negative: a needle present nowhere returns nothing.
        assert_eq!(search(&store, "zzzzz", None).unwrap()["total"], 0);
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
