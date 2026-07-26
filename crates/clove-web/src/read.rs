//! Read endpoints.
//!
//! The item lists go through [`clove_engine::Engine`], the single daemon → index
//! → files cascade the CLI and MCP tools also use, so a result here matches
//! `clove ls`/`ready`/`blocked` exactly and `_meta.source` names the tier that
//! answered. This endpoint used to read files unconditionally *and* rebuild the
//! whole dependency graph per request, while reporting the serving mode
//! (`"standalone"`/`"daemon"`) in the field that everywhere else names a tier
//! (read-path roadmap §4).
//!
//! Engine calls are blocking (SQLite, a daemon RPC on its own runtime, file
//! parsing), so they run on `spawn_blocking` rather than on an axum worker.

use std::collections::{BTreeSet, HashMap};

use axum::extract::{Path, Query, State};
use clove_core::StatsOptions;
use clove_engine::{ListAnswer, Projection, Rows};
use clove_types::{CloveId, ItemFrontmatter};
use serde_json::{json, Value};

use crate::dto::{frontmatter_value, item_value, with_terms, GraphContext};
use crate::error::{ok, ok_data, ApiError, ApiResult};
use crate::AppState;

/// Run a blocking engine call off the axum worker threads.
///
/// The engine reads SQLite, may drive a daemon RPC on its own tokio runtime, and
/// parses item files — none of which may happen on an async worker. (`cloved`
/// hosts this server on a two-worker runtime, so one blocking read there would
/// stall `ping`, the watcher, and every other request.)
async fn blocking<T, F>(f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => Err(ApiError::from(clove_types::CloveError::Io {
            path: camino::Utf8PathBuf::from("<engine>"),
            source: std::io::Error::other(e.to_string()),
        })),
    }
}

/// Render an engine answer as the web's item objects.
///
/// The three tiers produce the same row shape by different routes: the file tier
/// hands back the graph it built, so `ready`/`blocked_by`/`dangling_deps` come
/// from the whole-store partition as before, while an index or daemon answer has
/// no graph and derives the same three values per item from its own dependency
/// closure (`ops::graph_terms_detailed`). Both go through
/// [`crate::dto::with_terms`], so the row cannot differ by tier.
fn values_of(answer: ListAnswer, state: &AppState) -> Result<Vec<Value>, ApiError> {
    let ListAnswer { rows, graph, .. } = answer;
    let rows = match rows {
        Rows::Full(rows) => rows,
        // Unreachable: every read here asks for `Projection::Full`, because the
        // lean five columns carry none of the graph terms this API renders.
        Rows::Lean(_) => return Ok(Vec::new()),
    };
    match graph {
        Some(graph) => {
            let ctx = GraphContext::from_graph(graph);
            Ok(rows
                .iter()
                .map(|row| Value::Object(frontmatter_value(&row.frontmatter, &ctx)))
                .collect())
        }
        None => rows
            .iter()
            .map(|row| {
                let terms = clove_core::ops::graph_terms_detailed(&state.store, &row.frontmatter)?;
                Ok(Value::Object(with_terms(&row.frontmatter, &terms)))
            })
            .collect(),
    }
}

/// Parse `?id=` style path segments into a validated [`CloveId`].
fn parse_id(raw: &str) -> Result<CloveId, ApiError> {
    CloveId::new(raw).map_err(ApiError::from)
}

/// Split a repeated/csv query value (`a,b,c`) into trimmed, non-empty parts,
/// through the shared splitter so the CLI/MCP/web spellings decode alike.
fn csv(params: &HashMap<String, String>, key: &str) -> Vec<String> {
    clove_core::view::Filters::split_csv(params.get(key).map(String::as_str))
}

/// How a read result is shaped before it goes on the wire: `?fields=` and
/// `?compact=`, the last result-shaping gap between the web and the other two
/// surfaces (read-path roadmap §5).
///
/// The semantics are the **CLI's**, not the MCP server's: both default off, so
/// an unshaped request returns exactly the object it always did. (`clove-mcp`
/// compacts by default because it is spending a model's context window; a
/// browser sending no parameters must keep every key, since the SPA reads
/// `assignee: null` and `labels: []` as answers.)
///
/// `fields` is honoured literally — `?fields=assignee` on an unassigned item
/// still yields `{"assignee": null}`, so a caller can tell "unset" from "not
/// requested" — and `compact` composes on top of it, exactly as
/// `clove ls --fields … --compact` does.
#[derive(Debug, Clone, Default)]
struct Shape {
    fields: Option<Vec<String>>,
    compact: bool,
}

impl Shape {
    /// Whether this shape would change any object (the fast path is `false`).
    fn is_noop(&self) -> bool {
        self.fields.is_none() && !self.compact
    }

    /// Shape one item object.
    fn apply(&self, obj: serde_json::Map<String, Value>) -> Value {
        let obj = match &self.fields {
            Some(fields) => clove_core::view::project(obj, fields),
            None => obj,
        };
        let obj = if self.compact {
            // `compact_read` (not bare `compact`) so `schema` — the per-file
            // migration marker every surface drops — goes with it.
            clove_core::view::compact_read(obj)
        } else {
            obj
        };
        Value::Object(obj)
    }

    /// Shape a list of already-rendered item objects.
    fn apply_all(&self, values: Vec<Value>) -> Vec<Value> {
        if self.is_noop() {
            return values;
        }
        values
            .into_iter()
            .map(|v| match v {
                Value::Object(obj) => self.apply(obj),
                other => other,
            })
            .collect()
    }
}

/// Parse a boolean query parameter strictly.
///
/// Absent or empty is `false`; `true`/`1` and `false`/`0` are the accepted
/// spellings; anything else is a `VALIDATION_ERROR` rather than a silent
/// `false` — the same treatment `?sort=` and `?status=` get, and the reason is
/// the same: `?compact=yes` quietly returning the full shape is a result a
/// client cannot distinguish from a server that does not support the parameter.
fn bool_param(params: &HashMap<String, String>, key: &str) -> Result<bool, ApiError> {
    match params.get(key).map(String::as_str) {
        None | Some("") | Some("false") | Some("0") => Ok(false),
        Some("true") | Some("1") => Ok(true),
        Some(other) => Err(ApiError::from(clove_types::CloveError::InvalidField {
            field: key.to_owned(),
            reason: format!("expected true or false, got `{other}`"),
        })),
    }
}

/// The requested `?fields=`/`?compact=`.
fn shape_of(params: &HashMap<String, String>) -> Result<Shape, ApiError> {
    let fields = match csv(params, "fields") {
        f if f.is_empty() => None,
        f => Some(f),
    };
    Ok(Shape {
        fields,
        compact: bool_param(params, "compact")?,
    })
}

/// Parse a whole-number query parameter strictly.
///
/// Absent or empty is `None` (the caller's default); anything that is not a
/// non-negative decimal integer is a `VALIDATION_ERROR`, for exactly the reason
/// [`bool_param`] gives. This is the roadmap's §7 "malformed query value" item:
/// `?limit=abc` and `?limit=-5` used to fall through `.ok()` to the *default*,
/// which on the web is **unlimited** — so a client typo returned the whole store
/// with a 200, and `?offset=-1` silently became `0`. The CLI rejects the same
/// input (clap parses `--limit` as a `usize`), and `?sort=`/`?status=`/
/// `?compact=` on this very endpoint reject theirs; only the numbers were lenient.
fn usize_param(params: &HashMap<String, String>, key: &str) -> Result<Option<usize>, ApiError> {
    match params.get(key).map(String::as_str) {
        None | Some("") => Ok(None),
        Some(raw) => raw.parse::<usize>().map(Some).map_err(|_| {
            ApiError::from(clove_types::CloveError::InvalidField {
                field: key.to_owned(),
                reason: format!("expected a whole number ≥ 0, got `{raw}`"),
            })
        }),
    }
}

/// Parse `?offset=`/`?limit=` through the shared contract.
///
/// `?limit=0` means **unlimited**, as it does on the CLI and MCP. It previously
/// meant "return nothing" here — the same parameter with the opposite meaning on
/// one surface out of three.
fn page_window(params: &HashMap<String, String>) -> Result<clove_core::view::Page, ApiError> {
    Ok(clove_core::view::Page::new(
        usize_param(params, "offset")?.unwrap_or(0),
        usize_param(params, "limit")?,
        clove_core::view::defaults::WEB_LIMIT,
    ))
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
/// nope` gives. Every other parameter on these endpoints now answers the same
/// way: the numbers go through [`usize_param`] and the flags through
/// [`bool_param`] (roadmap §7).
fn order_of(params: &HashMap<String, String>) -> Result<clove_core::view::Order, ApiError> {
    clove_core::view::Order::parse(
        params.get("sort").map(String::as_str),
        params.get("dir").map(String::as_str),
    )
    .map_err(ApiError::from)
}

/// Which list an `?mode=` request wants. Parsed strictly (see the call site).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListMode {
    All,
    Ready,
    Blocked,
}

/// `GET /api/v1/items` — filtered, sorted, paginated list.
///
/// `?mode=ready|blocked` selects the corresponding engine query, so the three
/// lists share one tiering decision with `clove ready`/`clove blocked` instead
/// of being a fourth in-memory reimplementation of the partition.
pub async fn list_items(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let order = order_of(&params)?;
    let filters = filters_of(&params)?;
    let shape = shape_of(&params)?;
    let window = page_window(&params)?;
    // Strict, like every other parameter on this endpoint. `?mode=redy` used to
    // fall through to the unfiltered list with a 200 — and `mode` is not echoed
    // in `_meta`, so a client could not tell a typo from a server that does not
    // implement the mode it asked for. That is the exact defect the rest of this
    // module was tightened to remove.
    let mode = match params.get("mode").map(String::as_str) {
        None | Some("") | Some("all") | Some("list") => ListMode::All,
        Some("ready") => ListMode::Ready,
        Some("blocked") => ListMode::Blocked,
        Some(other) => {
            return Err(ApiError::from(clove_types::CloveError::InvalidField {
                field: "mode".to_owned(),
                reason: format!("unknown mode `{other}` (expected ready, blocked, or all)"),
            }))
        }
    };

    let engine = state.engine.clone();
    let (f, w) = (filters.clone(), window);
    let answer = blocking(move || {
        // Full frontmatter: this API renders every field plus the graph terms,
        // which no lean row carries.
        let answer = match mode {
            ListMode::Ready => engine.ready(&f, order, w, Projection::Full),
            ListMode::Blocked => engine.blocked(&f, order, w, Projection::Full),
            ListMode::All => engine.list(&f, order, w, Projection::Full),
        };
        answer.map_err(ApiError::from)
    })
    .await?;

    let source = answer.source.as_str();
    let total = answer.total;
    let page = values_of(answer, &state)?;
    // `returned` counts rows, so it is taken before shaping — a projection
    // changes each row's keys, never how many rows came back.
    let returned = page.len();
    let page = shape.apply_all(page);

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
            "source": source,
        }),
    ))
}

/// `GET /api/v1/items/:id` — full item detail.
///
/// The graph terms come from the item's own dependency closure
/// (`ops::graph_terms_detailed`), not from a whole-store graph: rendering one
/// item used to scan and parse every file in the repo to learn whether *that*
/// item was ready, which is the per-request rebuild read-path §4 names.
pub async fn get_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let shape = shape_of(&params)?;
    let issues_dir = state.issues_dir.clone();
    let store = state.store.clone();
    let obj = blocking(move || {
        let item = store.get(&id)?;
        let terms = clove_core::ops::graph_terms_detailed(&store, &item.frontmatter)?;
        Ok(item_value(&item, &issues_dir, &terms))
    })
    .await?;
    Ok(ok(
        shape.apply(obj),
        json!({ "source": clove_engine::Source::Files.as_str() }),
    ))
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
        usize_param(&params, "skip_newest")?.unwrap_or(0),
        usize_param(&params, "limit")?,
        clove_core::view::defaults::WEB_LIMIT,
    );
    let engine = state.engine.clone();
    let page = blocking(move || engine.comments(&id, window).map_err(ApiError::from)).await?;
    let page = page.value;
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
    let depth = match usize_param(&params, "depth")?
        .unwrap_or(clove_core::view::defaults::DEP_TREE_DEPTH)
    {
        0 => usize::MAX,
        n => n,
    };
    // Tiered: a live daemon answers from its cached graph, so the common case
    // no longer scans the store to walk one item's subtree.
    let engine = state.engine.clone();
    let answer = blocking(move || engine.dep_tree(&id, depth).map_err(ApiError::from)).await?;
    Ok(ok(
        answer.value,
        json!({ "source": answer.source.as_str() }),
    ))
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
    let order = order_of(&params)?;
    let filters = filters_of(&params)?;
    let shape = shape_of(&params)?;

    // The board shows every matching item grouped into columns, so the query is
    // unwindowed: `limit`/`offset` window each *column* below, not the query.
    let engine = state.engine.clone();
    let (f, unwindowed) = (filters.clone(), clove_core::view::Page::unlimited());
    let answer = blocking(move || {
        engine
            .list(&f, order, unwindowed, Projection::Full)
            .map_err(ApiError::from)
    })
    .await?;
    let source = answer.source.as_str();
    let selected = values_of(answer, &state)?;

    let mut columns: Vec<(&str, &str, Vec<Value>)> = vec![
        ("open", "Open", Vec::new()),
        ("in_progress", "In Progress", Vec::new()),
        ("closed", "Closed", Vec::new()),
    ];
    for value in selected {
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Some(col) = columns.iter_mut().find(|c| c.0 == status) {
            col.2.push(value);
        }
    }
    let window = page_window(&params)?;
    let columns: Vec<Value> = columns
        .into_iter()
        .map(|(key, label, items)| {
            let (page, count) = window.apply(items);
            let returned = page.len();
            // Shaping runs *after* the grouping, which reads each row's
            // `status`: projecting first would let `?fields=id` empty every
            // column instead of returning ids.
            json!({
                "key": key,
                "label": label,
                "count": count,
                "returned": returned,
                "items": shape.apply_all(page),
            })
        })
        .collect();
    Ok(ok(
        json!({ "columns": columns }),
        json!({
            "source": source,
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
    let opts = StatsOptions {
        top: usize_param(&params, "top")?.unwrap_or(clove_core::view::defaults::STATS_TOP),
        // Through `bool_param`, like every other flag here: this was a raw
        // `== Some("true")`, so `?no_epics=1` silently *kept* the epic rollup —
        // the spelling the same client would use for `?compact=1`.
        include_epics: !bool_param(&params, "no_epics")?,
    };
    let engine = state.engine.clone();
    let answer = blocking(move || {
        engine
            .stats(opts.top, opts.include_epics, chrono::Utc::now())
            .map_err(ApiError::from)
    })
    .await?;
    Ok(ok(
        answer.value,
        json!({ "source": answer.source.as_str() }),
    ))
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
    // Parsed by the caller and passed in: parsing it here too would make a
    // malformed `?limit=` reject or not depending on whether the repo happened
    // to have snapshots recorded.
    window: clove_core::view::Page,
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

    // The window is parsed *before* either path runs, so a malformed `?limit=`
    // is a 422 whether or not this repo has recorded snapshots.
    let window = page_window(&params)?;

    // Durable recorded snapshots win when present.
    if let Some((points, total)) = recorded_history_points(&state, &params, window) {
        let recorded = points.len();
        return Ok(ok(
            json!(points),
            json!({
                // The tier that answered, like every other read endpoint —
                // recorded snapshots live in `index.db`. This reported
                // `state.source` (the serving mode, `standalone`/`daemon`) after
                // the rest of the API moved to naming the tier, so one endpoint
                // returned a value outside the enum the published schema lists.
                "source": clove_engine::Source::Index.as_str(),
                "synthesized": false,
                "snapshots": recorded,
                "total": total,
                "returned": recorded,
                "offset": window.offset,
                "limit": window.reported_limit(),
            }),
        ));
    }

    // `?days=` is strict for the same reason as `?limit=` (a typo used to mean
    // "90 days" silently); the clamp then bounds a legal-but-absurd request.
    let days: i64 = usize_param(&params, "days")?
        .map(|n| n.min(i64::MAX as usize) as i64)
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
        // Synthesized from the item files, so: `files`.
        json!({ "source": clove_engine::Source::Files.as_str(), "synthesized": true }),
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
