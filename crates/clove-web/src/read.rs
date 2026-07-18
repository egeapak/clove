//! Read endpoints. All read from the file store + the in-memory graph (files are
//! truth), so results match the CLI's `ls`/`ready`/`blocked`/`show` exactly.

use std::collections::{BTreeSet, HashMap};

use axum::extract::{Path, Query, State};
use clove_core::{compute_stats, list_comments, GraphStore, StatsOptions};
use clove_types::{CloveId, ItemFrontmatter};
use serde_json::{json, Value};

use crate::dto::{frontmatter_value, item_value, GraphContext};
use crate::error::{ok, ok_data, ApiError, ApiResult};
use crate::AppState;

/// Parse `?id=` style path segments into a validated [`CloveId`].
fn parse_id(raw: &str) -> Result<CloveId, ApiError> {
    CloveId::new(raw).map_err(ApiError::from)
}

/// Split a repeated/csv query value (`a,b,c`) into trimmed, non-empty parts.
fn csv(params: &HashMap<String, String>, key: &str) -> Vec<String> {
    params
        .get(key)
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Load the whole store's frontmatter and the derived graph context.
fn load(state: &AppState) -> Result<(Vec<ItemFrontmatter>, GraphContext), ApiError> {
    let (frontmatters, _errors) = state.store.scan_frontmatter()?;
    let ctx = GraphContext::build(&frontmatters);
    Ok((frontmatters, ctx))
}

/// Whether `fm` passes the query filters (status/type/priority OR within a field;
/// labels AND; assignee exact; `q` substring over id/title/labels).
fn matches(fm: &ItemFrontmatter, params: &HashMap<String, String>) -> bool {
    let statuses = csv(params, "status");
    if !statuses.is_empty() && !statuses.iter().any(|s| s == fm.status.as_str()) {
        return false;
    }
    let types = csv(params, "type");
    if !types.is_empty() && !types.iter().any(|t| t == fm.item_type.as_str()) {
        return false;
    }
    let priorities = csv(params, "priority");
    if !priorities.is_empty()
        && !priorities
            .iter()
            .any(|p| p == &fm.priority.get().to_string())
    {
        return false;
    }
    if let Some(assignee) = params.get("assignee").filter(|s| !s.is_empty()) {
        if fm.assignee.as_deref() != Some(assignee.as_str()) {
            return false;
        }
    }
    // Labels are AND: every requested label must be present.
    for label in csv(params, "label") {
        if !fm.labels.iter().any(|l| l == &label) {
            return false;
        }
    }
    if let Some(q) = params.get("q").filter(|s| !s.is_empty()) {
        let needle = q.to_lowercase();
        let hay = format!(
            "{} {} {}",
            fm.id.as_str().to_lowercase(),
            fm.title.to_lowercase(),
            fm.labels.join(" ").to_lowercase()
        );
        if !hay.contains(&needle) {
            return false;
        }
    }
    true
}

/// Sort frontmatter in place by the requested `sort`/`dir` (default `rank`).
fn sort_items(items: &mut [ItemFrontmatter], params: &HashMap<String, String>, graph: &GraphStore) {
    let field = params.get("sort").map(String::as_str).unwrap_or("rank");
    let desc = params.get("dir").map(String::as_str) == Some("desc");
    let ranks = graph.topological_ranks();

    items.sort_by(|a, b| {
        let ord = match field {
            "priority" => a
                .priority
                .get()
                .cmp(&b.priority.get())
                .then_with(|| a.id.cmp(&b.id)),
            "created" => a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)),
            "updated" => a.updated.cmp(&b.updated).then_with(|| a.id.cmp(&b.id)),
            "id" => a.id.cmp(&b.id),
            // "rank" (default): (priority, topo rank, id).
            _ => a
                .priority
                .get()
                .cmp(&b.priority.get())
                .then_with(|| {
                    let ra = ranks.get(&a.id).copied().unwrap_or(usize::MAX);
                    let rb = ranks.get(&b.id).copied().unwrap_or(usize::MAX);
                    ra.cmp(&rb)
                })
                .then_with(|| a.id.cmp(&b.id)),
        };
        if desc {
            ord.reverse()
        } else {
            ord
        }
    });
}

/// `GET /api/v1/items` — filtered, sorted, paginated list.
pub async fn list_items(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let (frontmatters, ctx) = load(&state)?;
    let mode = params.get("mode").map(String::as_str).unwrap_or("list");

    let mut selected: Vec<ItemFrontmatter> = frontmatters
        .into_iter()
        .filter(|fm| matches(fm, &params))
        .filter(|fm| match mode {
            "ready" => ctx.is_ready(&fm.id),
            "blocked" => ctx.is_blocked(&fm.id),
            _ => true,
        })
        .collect();

    sort_items(&mut selected, &params, ctx.graph());

    let total = selected.len();
    let offset = params
        .get("offset")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(usize::MAX);

    let page: Vec<Value> = selected
        .iter()
        .skip(offset)
        .take(limit)
        .map(|fm| Value::Object(frontmatter_value(fm, &ctx)))
        .collect();
    let returned = page.len();

    Ok(ok(
        json!(page),
        json!({ "total": total, "returned": returned, "offset": offset, "source": state.source }),
    ))
}

/// `GET /api/v1/items/:id` — full item detail.
pub async fn get_item(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = parse_id(&id)?;
    let item = state.store.get(&id)?;
    let (_frontmatters, ctx) = load(&state)?;
    let obj = item_value(&item, &state.issues_dir, &ctx);
    Ok(ok(Value::Object(obj), json!({ "source": state.source })))
}

/// `GET /api/v1/items/:id/comments`.
pub async fn get_comments(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let id = parse_id(&id)?;
    if !state.store.exists(&id) {
        return Err(ApiError::from(clove_types::CloveError::NotFound {
            id: id.to_string(),
        }));
    }
    let mut comments = list_comments(&state.issues_dir, &id)?;
    if let Some(limit) = params.get("limit").and_then(|s| s.parse::<usize>().ok()) {
        comments.truncate(limit);
    }
    let data: Vec<Value> = comments
        .into_iter()
        .map(|c| json!({ "timestamp": c.timestamp.to_rfc3339(), "author": c.author, "body": c.body }))
        .collect();
    Ok(ok_data(json!(data)))
}

/// `GET /api/v1/items/:id/deptree?depth=`.
pub async fn get_deptree(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let depth = params
        .get("depth")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(5);
    let (_frontmatters, ctx) = load(&state)?;
    let tree = ctx
        .graph()
        .dep_tree(&id, depth)
        .ok_or_else(|| ApiError::from(clove_types::CloveError::NotFound { id: id.to_string() }))?;
    let value = serde_json::to_value(tree).unwrap_or(Value::Null);
    Ok(ok_data(value))
}

/// `GET /api/v1/board?group_by=status`.
pub async fn get_board(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let (frontmatters, ctx) = load(&state)?;
    let mut selected: Vec<ItemFrontmatter> = frontmatters
        .into_iter()
        .filter(|fm| matches(fm, &params))
        .collect();
    sort_items(&mut selected, &params, ctx.graph());

    let mut columns: Vec<(&str, &str, Vec<Value>)> = vec![
        ("open", "Open", Vec::new()),
        ("in_progress", "In Progress", Vec::new()),
        ("closed", "Closed", Vec::new()),
    ];
    for fm in &selected {
        let value = Value::Object(frontmatter_value(fm, &ctx));
        if let Some(col) = columns.iter_mut().find(|c| c.0 == fm.status.as_str()) {
            col.2.push(value);
        }
    }
    let columns: Vec<Value> = columns
        .into_iter()
        .map(|(key, label, items)| json!({ "key": key, "label": label, "count": items.len(), "items": items }))
        .collect();
    Ok(ok(
        json!({ "columns": columns }),
        json!({ "source": state.source }),
    ))
}

/// `GET /api/v1/stats`.
pub async fn get_stats(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let (frontmatters, ctx) = load(&state)?;
    let opts = StatsOptions {
        top: params
            .get("top")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(10),
        include_epics: params.get("no_epics").map(String::as_str) != Some("true"),
    };
    let report = compute_stats(&frontmatters, ctx.graph(), chrono::Utc::now(), opts);
    let value = serde_json::to_value(report).unwrap_or(Value::Null);
    Ok(ok_data(value))
}

/// `GET /api/v1/stats/history?days=N` — a daily throughput series
/// Recorded stats snapshots from `.clove/index.db`, mapped to history points
/// oldest→newest. `created`/`closed` are per-interval throughput deltas between
/// consecutive snapshots (the first point baselines at 0, since there is no prior
/// snapshot to difference against); `open`/`in_progress`/`total`/`ready`/`blocked`
/// are the real recorded levels at each capture — trends the file-synthesized
/// series cannot reconstruct. Returns `None` (so the caller synthesizes) when
/// there is no index or no snapshots. Honors `?since=<rfc3339>` and `?limit=N`.
fn recorded_history_points(
    state: &AppState,
    params: &HashMap<String, String>,
) -> Option<Vec<Value>> {
    use clove_index::Index;

    let db_path = state.issues_dir.parent()?.join("index.db");
    if !db_path.exists() {
        return None;
    }
    let index = Index::open(&db_path).ok()?;
    let since = params.get("since").map(String::as_str);
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0);
    // snapshot_history returns most-recent-first; reverse to chronological order
    // so the throughput deltas below run forward in time.
    let mut snapshots = index.snapshot_history(since, limit).ok()?;
    if snapshots.is_empty() {
        return None;
    }
    snapshots.reverse();

    let mut points = Vec::with_capacity(snapshots.len());
    let mut prev_totals: Option<(u64, u64)> = None; // (created_total, closed_total)
    for snap in &snapshots {
        let report = &snap.report;
        let (created, closed) = match prev_totals {
            Some((prev_created, prev_closed)) => (
                report.throughput.created_total.saturating_sub(prev_created),
                report.throughput.closed_total.saturating_sub(prev_closed),
            ),
            None => (0, 0),
        };
        prev_totals = Some((
            report.throughput.created_total,
            report.throughput.closed_total,
        ));
        let date = snap
            .captured_at
            .split('T')
            .next()
            .unwrap_or(&snap.captured_at)
            .to_owned();
        points.push(json!({
            "date": date,
            "captured_at": snap.captured_at,
            "created": created,
            "closed": closed,
            "open": report.by_status.open,
            "in_progress": report.by_status.in_progress,
            "total": report.total,
            "ready": report.ready,
            "blocked": report.blocked,
        }));
    }
    Some(points)
}

/// `GET /api/v1/stats/history` — the throughput/levels history for the timeline.
///
/// Prefers the durable snapshots recorded in `.clove/index.db` (real point-in-time
/// history, incl. ready/blocked levels — see [`recorded_history_points`]). When no
/// snapshots exist it falls back to a dense daily series (`{date, created, closed,
/// open}`) synthesized from item `created`/`closed` timestamps, always correct from
/// files alone; `open` is the running net-open count seeded from items predating the
/// window. `_meta.synthesized` tells the client which path produced the series.
pub async fn get_stats_history(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    use chrono::Duration;
    use std::collections::BTreeMap;

    // Durable recorded snapshots win when present.
    if let Some(points) = recorded_history_points(&state, &params) {
        let recorded = points.len();
        return Ok(ok(
            json!(points),
            json!({ "source": state.source, "synthesized": false, "snapshots": recorded }),
        ));
    }

    let days: i64 = params
        .get("days")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(90)
        .clamp(1, 365);

    let (frontmatters, _ctx) = load(&state)?;
    let today = chrono::Utc::now().date_naive();
    let window_start = today - Duration::days(days - 1);

    let mut created_by: BTreeMap<chrono::NaiveDate, i64> = BTreeMap::new();
    let mut closed_by: BTreeMap<chrono::NaiveDate, i64> = BTreeMap::new();
    // Items predating the window seed the running `open` baseline.
    let mut cum_created = 0i64;
    let mut cum_closed = 0i64;
    for fm in &frontmatters {
        let d = fm.created.date_naive();
        if d < window_start {
            cum_created += 1;
        } else {
            *created_by.entry(d).or_default() += 1;
        }
        if let Some(closed) = fm.closed {
            let cd = closed.date_naive();
            if cd < window_start {
                cum_closed += 1;
            } else {
                *closed_by.entry(cd).or_default() += 1;
            }
        }
    }

    let mut points = Vec::with_capacity(days as usize);
    for offset in (0..days).rev() {
        let date = today - Duration::days(offset);
        let created = created_by.get(&date).copied().unwrap_or(0);
        let closed = closed_by.get(&date).copied().unwrap_or(0);
        cum_created += created;
        cum_closed += closed;
        points.push(json!({
            "date": date.format("%Y-%m-%d").to_string(),
            "created": created,
            "closed": closed,
            "open": (cum_created - cum_closed).max(0),
        }));
    }

    Ok(ok(
        json!(points),
        json!({ "source": state.source, "synthesized": true }),
    ))
}

/// `GET /api/v1/meta` — bootstraps the filter dropdowns and create form.
pub async fn get_meta(State(state): State<AppState>) -> ApiResult {
    let (frontmatters, _ctx) = load(&state)?;
    let mut labels: BTreeSet<String> = BTreeSet::new();
    let mut assignees: BTreeSet<String> = BTreeSet::new();
    for fm in &frontmatters {
        for l in &fm.labels {
            labels.insert(l.clone());
        }
        if let Some(a) = &fm.assignee {
            assignees.insert(a.clone());
        }
    }
    let data = json!({
        "id_prefix": state.id_prefix,
        "types": ["bug", "feature", "chore", "docs", "epic"],
        "statuses": ["open", "in_progress", "closed"],
        "priorities": [0, 1, 2, 3, 4],
        "labels": labels.into_iter().collect::<Vec<_>>(),
        "assignees": assignees.into_iter().collect::<Vec<_>>(),
        "daemon": { "running": state.daemon_running, "web_addr": Value::Null },
        "source": state.source,
    });
    Ok(ok_data(data))
}

/// `GET /api/v1/cycles` — hard-dependency cycles.
pub async fn get_cycles(State(state): State<AppState>) -> ApiResult {
    let (_frontmatters, ctx) = load(&state)?;
    let cycles: Vec<Vec<String>> = ctx
        .graph()
        .all_cycles()
        .into_iter()
        .map(|cycle| cycle.iter().map(CloveId::to_string).collect())
        .collect();
    Ok(ok_data(json!({ "cycles": cycles })))
}
