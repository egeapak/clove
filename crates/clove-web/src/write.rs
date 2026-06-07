//! Write endpoints. Every mutation goes through `clove_core::ItemStore` (atomic
//! rename + advisory lock), so web writes are concurrency-safe with the CLI and
//! daemon and re-enter the file-watcher → push loop. Each returns the updated item.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use chrono::Utc;
use clove_core::{add_comment as core_add_comment, apply_edit, ops};
use clove_types::{
    CloveError, CloveId, EditRequest, Item, ItemStatus, ItemType, LabelEdit, NewSpec, Priority,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::dto::{item_value, GraphContext};
use crate::error::{ok, ApiError, ApiResult};
use crate::AppState;

fn parse_id(raw: &str) -> Result<CloveId, ApiError> {
    CloveId::new(raw).map_err(ApiError::from)
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
    // Delegate all field parsing/validation to the shared core op (one
    // implementation across CLI/web/MCP/daemon), then render the full detail.
    let spec = NewSpec {
        title: body.title,
        item_type: body.r#type,
        priority: body.priority,
        labels: body.labels,
        deps: body.deps,
        parent: body.parent,
        assignee: body.assignee,
        body: body.body,
    };
    let created = ops::create(
        &state.store,
        &state.id_prefix,
        ItemType::default(),
        spec,
        Utc::now(),
    )?;
    let id = parse_id(created["id"].as_str().unwrap_or_default())?;
    respond_item(&state, &state.store.get(&id)?)
}

#[derive(Debug, Deserialize)]
pub struct PatchBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<u8>,
    #[serde(default)]
    pub assignee: Option<Option<String>>,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// When present, replaces the whole label set (form semantics). For
    /// incremental add/remove use `PUT /labels`.
    #[serde(default)]
    pub labels: Option<Vec<String>>,
}

/// `PATCH /api/v1/items/:id` — a structured partial edit through the shared
/// [`apply_edit`] path (one validation + status-invariant implementation).
pub async fn patch_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: ApiJson<PatchBody>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let body = body.0;

    let req = EditRequest {
        title: body.title,
        body: body.body,
        status: body
            .status
            .map(|s| ItemStatus::parse(&s))
            .transpose()
            .map_err(ApiError::from)?,
        priority: body
            .priority
            .map(Priority::new)
            .transpose()
            .map_err(ApiError::from)?,
        item_type: body
            .r#type
            .map(|t| ItemType::parse(&t))
            .transpose()
            .map_err(ApiError::from)?,
        // Tri-state: absent → leave; `null`/empty → clear; value → set. Map an
        // empty string to a clear so a form submitting "" doesn't trip the
        // empty-assignee guard.
        assignee: body.assignee.map(|a| a.filter(|s| !s.trim().is_empty())),
        labels: body.labels.map(LabelEdit::Set),
    };

    apply_edit(&state.store, &id, &req, Utc::now())?;
    respond_item(&state, &state.store.get(&id)?)
}

#[derive(Debug, Deserialize)]
pub struct LabelsBody {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

/// `PUT /api/v1/items/:id/labels` — incremental add/remove via the shared path.
pub async fn put_labels(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: ApiJson<LabelsBody>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let body = body.0;
    let req = EditRequest {
        labels: Some(LabelEdit::Delta {
            add: body.add,
            remove: body.remove,
        }),
        ..Default::default()
    };
    apply_edit(&state.store, &id, &req, Utc::now())?;
    respond_item(&state, &state.store.get(&id)?)
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
    // A single-token author slug round-trips cleanly through the comment-filename
    // parser (`<ts>-<author>-<rand>.md`); an email with punctuation would not.
    let author = body
        .author
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| "web".to_owned());
    core_add_comment(&state.issues_dir, &id, &author, &body.body)?;
    let item = state.store.get(&id)?;
    respond_item(&state, &item)
}

#[derive(Debug, Deserialize)]
pub struct DepBody {
    pub dep: String,
}

/// `POST /api/v1/items/:id/deps` — add a hard dependency. The full existence /
/// self-loop / duplicate / cycle validation lives in the shared core op.
pub async fn add_dep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: ApiJson<DepBody>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let dep = parse_id(&body.0.dep)?;
    ops::dep_add(&state.store, &id, &dep, Utc::now())?;
    respond_item(&state, &state.store.get(&id)?)
}

/// `DELETE /api/v1/items/:id/deps/:dep`. Idempotent (HTTP DELETE semantics): a
/// no-op if the dependency isn't present.
pub async fn remove_dep(
    State(state): State<AppState>,
    Path((id, dep)): Path<(String, String)>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let dep = parse_id(&dep)?;
    if state.store.get(&id)?.frontmatter.deps.contains(&dep) {
        ops::dep_remove(&state.store, &id, &dep, Utc::now())?;
    }
    respond_item(&state, &state.store.get(&id)?)
}

#[derive(Debug, Deserialize)]
pub struct ParentBody {
    /// The new parent id, or `null` to clear the parent.
    #[serde(default)]
    pub parent: Option<String>,
}

/// `PUT /api/v1/items/:id/parent` — set or clear the parent (epic membership),
/// with existence + parent-cycle validation in the shared core op.
pub async fn put_parent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: ApiJson<ParentBody>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let parent = match &body.0.parent {
        Some(p) => Some(parse_id(p)?),
        None => None,
    };
    ops::set_parent(&state.store, &id, parent.as_ref(), Utc::now())?;
    respond_item(&state, &state.store.get(&id)?)
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
