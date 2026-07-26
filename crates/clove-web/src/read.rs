//! Read endpoints. All read from the file store + the in-memory graph (files are
//! truth), so results match the CLI's `ls`/`ready`/`blocked`/`show` exactly.

use std::collections::{BTreeSet, HashMap};

use axum::extract::{Path, Query, State};
use clove_core::{compute_stats, GraphStore, StatsOptions};
use clove_types::{CloveId, ItemFrontmatter};
use serde_json::{json, Value};

use crate::dto::{frontmatter_value, item_value, GraphContext};
use crate::error::{ok, ok_data, ApiError, ApiResult};
use crate::AppState;

/// Parse `?id=` style path segments into a validated [`CloveId`].
fn parse_id(raw: &str) -> Result<CloveId, ApiError> {
    CloveId::new(raw).map_err(ApiError::from)
}

/// Split a repeated/csv query value (`a,b,c`) into trimmed, non-empty parts,
/// through the shared splitter so the CLI/MCP/web spellings decode alike.
fn csv(params: &HashMap<String, String>, key: &str) -> Vec<String> {
    clove_core::view::Filters::split_csv(params.get(key).map(String::as_str))
}

/// Parse `?offset=`/`?limit=` through the shared contract.
///
/// `?limit=0` means **unlimited**, as it does on the CLI and MCP. It previously
/// meant "return nothing" here — the same parameter with the opposite meaning on
/// one surface out of three.
fn page_window(params: &HashMap<String, String>) -> clove_core::view::Page {
    clove_core::view::Page::new(
        params
            .get("offset")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0),
        params.get("limit").and_then(|s| s.parse::<usize>().ok()),
        clove_core::view::defaults::WEB_LIMIT,
    )
}

/// Load the whole store's frontmatter and the derived graph context.
fn load(state: &AppState) -> Result<(Vec<ItemFrontmatter>, GraphContext), ApiError> {
    let (frontmatters, _errors) = state.store.scan_frontmatter()?;
    let ctx = GraphContext::build(&frontmatters);
    Ok((frontmatters, ctx))
}

/// The requested `?status=`/`?type=`/`?priority=`/`?label=`/`?assignee=`/`?q=`,
/// through the shared contract.
///
/// This endpoint had the *only* multi-value filter implementation in the project
/// — a private predicate here comparing raw strings — while the CLI and MCP took
/// one value per field. It is now `clove_core::view::Filters`, which every
/// surface shares; the accepted spellings (csv values, AND-ed labels, `q` over
/// id/title/labels) are unchanged, and the CLI/MCP gained them rather than the
/// web losing anything.
///
/// Two deliberate differences from the predicate this replaces:
///
/// - an unrecognized value is a `VALIDATION_ERROR` rather than a filter that
///   matches nothing (`?status=bogus` used to return `[]`, indistinguishable
///   from "no open bugs"), the same treatment `?sort=` already gets;
/// - `q` is matched against id, title, and each label *separately*, where the
///   old code concatenated the three into one haystack — so a needle containing
///   a space can no longer match across a field boundary (`?q=x%20y` matching an
///   id ending `x` beside a title starting `y`). That was an artefact of the
///   concatenation, not a feature.
fn filters_of(params: &HashMap<String, String>) -> Result<clove_core::view::Filters, ApiError> {
    let present = |key: &str| {
        params
            .get(key)
            .filter(|s| !s.is_empty())
            .map(String::as_str)
    };
    clove_core::view::Filters::parse_multi(
        &csv(params, "status"),
        &csv(params, "type"),
        &csv(params, "label"),
        present("assignee"),
        &csv(params, "priority"),
        present("q"),
    )
    .map_err(ApiError::from)
}

/// The requested `?sort=`/`?dir=`, through the shared contract.
///
/// This endpoint had the *only* sort implementation in the project — a private
/// comparator here, and no sort argument at all on the CLI or MCP. It is now
/// `clove_core::view::Order`, which every surface shares; the accepted spellings
/// (`rank|priority|created|updated|id`, `dir=desc`) are unchanged, and
/// `status`/`type` come along with the shared enum.
///
/// Unlike the old comparator, an unrecognized value is a `VALIDATION_ERROR`
/// rather than a silent fall-back to `rank` — the same answer `clove ls --sort
/// nope` gives. (`?limit=abc` is still lenient; making the *whole* query string
/// strict is roadmap §7.)
fn order_of(params: &HashMap<String, String>) -> Result<clove_core::view::Order, ApiError> {
    clove_core::view::Order::parse(
        params.get("sort").map(String::as_str),
        params.get("dir").map(String::as_str),
    )
    .map_err(ApiError::from)
}

/// Sort frontmatter in place by `order`.
fn sort_items(items: &mut [ItemFrontmatter], order: clove_core::view::Order, graph: &GraphStore) {
    // `rank` is the only field that reads the graph, and the caller has already
    // built it; every other field keys off the frontmatter alone.
    let ranks = if order.needs_ranks() {
        graph.topological_ranks()
    } else {
        HashMap::new()
    };
    order.apply(items, &ranks);
}

/// `GET /api/v1/items` — filtered, sorted, paginated list.
pub async fn list_items(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let (frontmatters, ctx) = load(&state)?;
    let order = order_of(&params)?;
    let filters = filters_of(&params)?;
    let mode = params.get("mode").map(String::as_str).unwrap_or("list");

    let mut selected: Vec<ItemFrontmatter> = frontmatters
        .into_iter()
        .filter(|fm| filters.matches(fm))
        .filter(|fm| match mode {
            "ready" => ctx.is_ready(&fm.id),
            "blocked" => ctx.is_blocked(&fm.id),
            _ => true,
        })
        .collect();

    sort_items(&mut selected, order, ctx.graph());

    let window = page_window(&params);
    let rows: Vec<Value> = selected
        .iter()
        .map(|fm| Value::Object(frontmatter_value(fm, &ctx)))
        .collect();
    let (page, total) = window.apply(rows);
    let returned = page.len();

    Ok(ok(
        json!(page),
        json!({
            "total": total,
            "returned": returned,
            "offset": window.offset,
            "limit": window.reported_limit(),
            "sort": order.field.as_str(),
            "dir": order.dir_str(),
            "filters": serde_json::to_value(&filters).unwrap_or(Value::Null),
            "source": state.source,
        }),
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
    // Shared with `clove comments` and the `clove_comments` MCP tool. This
    // endpoint used to `truncate`, keeping the *oldest* n while both other
    // surfaces kept the newest — the same flag name with the opposite meaning.
    //
    // The window is anchored at the newest end, so the skip parameter is
    // `skip_newest`, not `offset` — the same spelling the CLI and MCP use. The
    // default is the *web* default (unlimited), like every other read here: the
    // SPA sends no limit and renders the thread against `comment_count`, so a
    // cap would show a full count above a truncated list.
    let window = clove_core::view::Page::new(
        params
            .get("skip_newest")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0),
        params.get("limit").and_then(|s| s.parse::<usize>().ok()),
        clove_core::view::defaults::WEB_LIMIT,
    );
    let page = clove_core::ops::comments(&state.store, &id, window)?;
    let data = page
        .get("items")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    Ok(ok(
        data,
        json!({
            "total": page["total"],
            "returned": page["returned"],
            "skip_newest": page["skip_newest"],
            "limit": page["limit"],
        }),
    ))
}

/// `GET /api/v1/items/:id/deptree?depth=`.
pub async fn get_deptree(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let id = parse_id(&id)?;
    // `?depth=0` is unlimited, matching `--depth 0` and every other bound.
    let depth = match params
        .get("depth")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(clove_core::view::defaults::DEP_TREE_DEPTH)
    {
        0 => usize::MAX,
        n => n,
    };
    let (_frontmatters, ctx) = load(&state)?;
    let tree = ctx
        .graph()
        .dep_tree(&id, depth)
        .ok_or_else(|| ApiError::from(clove_types::CloveError::NotFound { id: id.to_string() }))?;
    let value = serde_json::to_value(tree).unwrap_or(Value::Null);
    Ok(ok_data(value))
}

/// `GET /api/v1/board?group_by=status`.
///
/// `limit`/`offset` window each column **independently** — a board caps how tall
/// a column gets, which is the only reading of a single limit over grouped
/// columns that means anything. It previously accepted both (it shares
/// `matches`/`sort_items` with the item list) and silently dropped them.
///
/// `count` stays the column's full size, so a header reading "Closed · 412"
/// over 50 visible cards is honest rather than wrong; `returned` is what came
/// back.
pub async fn get_board(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let (frontmatters, ctx) = load(&state)?;
    let order = order_of(&params)?;
    let filters = filters_of(&params)?;
    let mut selected: Vec<ItemFrontmatter> = frontmatters
        .into_iter()
        .filter(|fm| filters.matches(fm))
        .collect();
    sort_items(&mut selected, order, ctx.graph());

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
    let window = page_window(&params);
    let columns: Vec<Value> = columns
        .into_iter()
        .map(|(key, label, items)| {
            let (page, count) = window.apply(items);
            json!({
                "key": key,
                "label": label,
                "count": count,
                "returned": page.len(),
                "items": page,
            })
        })
        .collect();
    Ok(ok(
        json!({ "columns": columns }),
        json!({
            "source": state.source,
            "offset": window.offset,
            "limit": window.reported_limit(),
            "sort": order.field.as_str(),
            "dir": order.dir_str(),
            "filters": serde_json::to_value(&filters).unwrap_or(Value::Null),
            "per_column": true,
        }),
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
            .unwrap_or(clove_core::view::defaults::STATS_TOP),
        include_epics: params.get("no_epics").map(String::as_str) != Some("true"),
    };
    let report = compute_stats(&frontmatters, ctx.graph(), chrono::Utc::now(), opts);
    let value = serde_json::to_value(report).unwrap_or(Value::Null);
    Ok(ok_data(value))
}

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
) -> Option<(Vec<Value>, usize)> {
    use clove_index::Index;

    let db_path = state.issues_dir.parent()?.join("index.db");
    if !db_path.exists() {
        return None;
    }
    let index = Index::open(&db_path).ok()?;
    let since = params.get("since").map(String::as_str);
    // Fetch the whole series and window it here rather than pushing the limit
    // into SQL. Pushing it down cannot honour `?offset=` — which this endpoint
    // parsed and then dropped — and reports the truncated count as the total,
    // the same defect the CLI's `stats --history` had.
    let window = page_window(params);
    let snapshots = index.snapshot_history(since, None).ok()?;
    if snapshots.is_empty() {
        return None;
    }
    // `snapshot_history` returns most-recent-first, so the window is applied
    // here (keeping the newest N, as `--limit` does) and the survivors are then
    // reversed into chronological order for the throughput deltas below.
    let (mut snapshots, total) = window.apply(snapshots);
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
    Some((points, total))
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
    if let Some((points, total)) = recorded_history_points(&state, &params) {
        let window = page_window(&params);
        let recorded = points.len();
        return Ok(ok(
            json!(points),
            json!({
                "source": state.source,
                "synthesized": false,
                "snapshots": recorded,
                "total": total,
                "returned": recorded,
                "offset": window.offset,
                "limit": window.reported_limit(),
            }),
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
