//! Write endpoints. Every mutation goes through `clove_core::ItemStore` (atomic
//! rename + advisory lock), so web writes are concurrency-safe with the CLI and
//! daemon and re-enter the file-watcher → push loop. Each returns the updated item.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use chrono::Utc;
use clove_core::{
    add_comment as core_add_comment, normalize_label, CloveError, CloveId, GraphStore, Item,
    ItemStatus, ItemType, NewItem, Priority,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::dto::{item_value, GraphContext};
use crate::error::{ok, ApiError, ApiResult};
use crate::AppState;

fn parse_id(raw: &str) -> Result<CloveId, ApiError> {
    CloveId::new(raw).map_err(ApiError::from)
}

fn parse_status(s: &str) -> Result<ItemStatus, ApiError> {
    match s {
        "open" => Ok(ItemStatus::Open),
        "in_progress" => Ok(ItemStatus::InProgress),
        "closed" => Ok(ItemStatus::Closed),
        other => Err(ApiError::bad_request(format!("invalid status: {other}"))),
    }
}

fn parse_type(s: &str) -> Result<ItemType, ApiError> {
    match s {
        "bug" => Ok(ItemType::Bug),
        "feature" => Ok(ItemType::Feature),
        "chore" => Ok(ItemType::Chore),
        "docs" => Ok(ItemType::Docs),
        "epic" => Ok(ItemType::Epic),
        other => Err(ApiError::bad_request(format!("invalid type: {other}"))),
    }
}

/// Build the updated-item response (full detail, with fresh graph context).
fn respond_item(state: &AppState, item: &Item) -> ApiResult {
    let (frontmatters, _errors) = state.store.scan_frontmatter()?;
    let ctx = GraphContext::build(&frontmatters);
    let obj = item_value(item, &state.issues_dir, &ctx);
    Ok(ok(Value::Object(obj), json!({ "source": state.source })))
}

#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub title: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub priority: Option<u8>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

/// `POST /api/v1/items`.
pub async fn create_item(State(state): State<AppState>, body: ApiJson<CreateBody>) -> ApiResult {
    let body = body.0;
    let item_type = match &body.r#type {
        Some(t) => parse_type(t)?,
        None => ItemType::default(),
    };
    let priority = match body.priority {
        Some(p) => Priority::new(p)?,
        None => Priority::DEFAULT,
    };
    let mut labels = Vec::new();
    for raw in &body.labels {
        labels.push(normalize_label(raw)?);
    }
    labels.sort();
    labels.dedup();
    let mut deps = Vec::new();
    for raw in &body.deps {
        deps.push(parse_id(raw)?);
    }
    let parent = match &body.parent {
        Some(p) => Some(parse_id(p)?),
        None => None,
    };
    let spec = NewItem {
        title: body.title,
        item_type,
        priority,
        labels,
        deps,
        parent,
        assignee: body.assignee,
        body: body.body.unwrap_or_default(),
    };
    let item = state.store.create(&state.id_prefix, spec, Utc::now())?;
    respond_item(&state, &item)
}

#[derive(Debug, Deserialize)]
pub struct PatchBody {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<u8>,
    #[serde(default)]
    pub assignee: Option<Option<String>>,
    #[serde(default)]
    pub r#type: Option<String>,
}

/// `PATCH /api/v1/items/:id`.
pub async fn patch_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: ApiJson<PatchBody>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let mut item = state.store.get(&id)?;
    let body = body.0;

    if let Some(s) = &body.status {
        let status = parse_status(s)?;
        item.frontmatter.status = status;
        // Maintain the status ↔ closed-timestamp invariant (DESIGN §2.3).
        match status {
            ItemStatus::Closed => {
                if item.frontmatter.closed.is_none() {
                    item.frontmatter.closed = Some(Utc::now());
                }
            }
            _ => item.frontmatter.closed = None,
        }
    }
    if let Some(p) = body.priority {
        item.frontmatter.priority = Priority::new(p)?;
    }
    if let Some(assignee) = body.assignee {
        item.frontmatter.assignee = assignee.filter(|s| !s.is_empty());
    }
    if let Some(t) = &body.r#type {
        item.frontmatter.item_type = parse_type(t)?;
    }

    let updated = state.store.update(&item, Utc::now())?;
    respond_item(&state, &updated)
}

#[derive(Debug, Deserialize)]
pub struct LabelsBody {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

/// `PUT /api/v1/items/:id/labels`.
pub async fn put_labels(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: ApiJson<LabelsBody>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let mut item = state.store.get(&id)?;
    let body = body.0;

    for raw in &body.remove {
        let label = normalize_label(raw)?;
        item.frontmatter.labels.retain(|l| l != &label);
    }
    for raw in &body.add {
        let label = normalize_label(raw)?;
        if !item.frontmatter.labels.contains(&label) {
            item.frontmatter.labels.push(label);
        }
    }
    item.frontmatter.labels.sort();
    item.frontmatter.labels.dedup();

    let updated = state.store.update(&item, Utc::now())?;
    respond_item(&state, &updated)
}

#[derive(Debug, Deserialize)]
pub struct CommentBody {
    pub body: String,
    #[serde(default)]
    pub author: Option<String>,
}

/// `POST /api/v1/items/:id/comments`.
pub async fn add_comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: ApiJson<CommentBody>,
) -> ApiResult {
    let id = parse_id(&id)?;
    if !state.store.exists(&id) {
        return Err(ApiError::from(CloveError::NotFound { id: id.to_string() }));
    }
    let body = body.0;
    let author = body.author.unwrap_or_else(|| "web@clove".to_owned());
    core_add_comment(&state.issues_dir, &id, &author, &body.body)?;
    let item = state.store.get(&id)?;
    respond_item(&state, &item)
}

#[derive(Debug, Deserialize)]
pub struct DepBody {
    pub dep: String,
}

/// `POST /api/v1/items/:id/deps` — add a hard dependency (with cycle pre-check).
pub async fn add_dep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: ApiJson<DepBody>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let dep = parse_id(&body.0.dep)?;

    let mut item = state.store.get(&id)?;
    // 1. dep must exist.
    if !state.store.exists(&dep) {
        return Err(ApiError::from(CloveError::NotFound {
            id: dep.to_string(),
        }));
    }
    // 2. no self-loop.
    if dep == id {
        return Err(ApiError::from(CloveError::SelfDependency {
            id: id.to_string(),
        }));
    }
    // 3. already present?
    if item.frontmatter.deps.contains(&dep) {
        return Err(ApiError::from(CloveError::DependencyExists {
            from: id.to_string(),
            to: dep.to_string(),
        }));
    }
    // 4. would-cycle check over the whole graph.
    let (frontmatters, _errors) = state.store.scan_frontmatter()?;
    let (graph, _dangling) = GraphStore::build(&frontmatters);
    if graph.check_would_cycle(&id, &dep) {
        return Err(ApiError::from(CloveError::DependencyCycle {
            from: id.to_string(),
            to: dep.to_string(),
            cycle: vec![id.to_string(), dep.to_string()],
        }));
    }

    item.frontmatter.deps.push(dep);
    item.frontmatter.deps.sort();
    item.frontmatter.deps.dedup();
    let updated = state.store.update(&item, Utc::now())?;
    respond_item(&state, &updated)
}

/// `DELETE /api/v1/items/:id/deps/:dep`.
pub async fn remove_dep(
    State(state): State<AppState>,
    Path((id, dep)): Path<(String, String)>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let dep = parse_id(&dep)?;
    let mut item = state.store.get(&id)?;
    item.frontmatter.deps.retain(|d| d != &dep);
    let updated = state.store.update(&item, Utc::now())?;
    respond_item(&state, &updated)
}

/// `DELETE /api/v1/items/:id?force=`.
pub async fn delete_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let force = params.get("force").map(String::as_str) == Some("true");
    state.store.delete(&id, force)?;
    Ok(ok(
        json!({ "id": id.to_string(), "deleted": true }),
        json!({ "source": state.source }),
    ))
}

/// A JSON body extractor that maps deserialization failures to the clove error
/// envelope (a `400 USAGE_ERROR`) instead of axum's default plain-text rejection.
pub struct ApiJson<T>(pub T);

#[axum::async_trait]
impl<T, S> axum::extract::FromRequest<S> for ApiJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let axum::Json(value) = axum::Json::<T>::from_request(req, state)
            .await
            .map_err(|e| ApiError::bad_request(format!("invalid request body: {e}")))?;
        Ok(ApiJson(value))
    }
}
