//! Write endpoints. Every mutation goes through `clove_core::ItemStore` (atomic
//! rename + advisory lock), so web writes are concurrency-safe with the CLI and
//! daemon and re-enter the file-watcher → push loop. Each returns the updated item.
//!
//! Every handler runs its store work through [`blocking`]: a write takes the
//! store lock, reads and renames files, and `dep_add`/`set_parent` scan the
//! whole store for the cycle check — none of which may happen on an axum
//! worker (see [`crate::read`]).

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use chrono::Utc;
use clove_core::{add_comment as core_add_comment, apply_edit, ops};
use clove_types::{
    CloveError, CloveId, EditRequest, Item, ItemStatus, ItemType, LabelEdit, NewSpec, Priority,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::dto::{item_value, missing_refs_on_disk};
use crate::error::{ok, ApiError, ApiResult};
use crate::read::{blocking, bool_param};
use crate::AppState;

fn parse_id(raw: &str) -> Result<CloveId, ApiError> {
    CloveId::new(raw).map_err(ApiError::from)
}

/// Deserialize a present field (including explicit `null`) into the inner option,
/// wrapped in `Some`, so a PATCH can distinguish "clear" (`null`) from "leave"
/// (absent). Mirrors `clove_types::EditRequest`'s tri-state assignee handling.
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// Build the updated-item response (full detail, with fresh graph terms).
///
/// The terms come from the item's own dependency closure rather than a
/// whole-store graph build: a write response used to re-scan and re-parse every
/// file in the repo to report whether the one item just written was ready.
///
/// Blocking: call it inside [`blocking`].
fn item_object(state: &AppState, item: &Item) -> Result<Value, ApiError> {
    let terms = clove_core::ops::graph_terms_detailed(&state.store, &item.frontmatter)?;
    let missing = missing_refs_on_disk(&state.store, &item.frontmatter);
    Ok(Value::Object(item_value(
        item,
        &state.issues_dir,
        &terms,
        missing,
    )))
}

/// `_meta` of a write response.
///
/// `source` names the tier that answered, as on the read endpoints, and a
/// write response is always rendered from the item files. It used to carry the
/// serving mode (`standalone`/`daemon`): a value outside the published enum
/// under `clove serve`, and a claim of `daemon` under `cloved` for an answer
/// read straight off disk.
fn write_meta() -> Value {
    json!({ "source": clove_engine::Source::Files.as_str() })
}

/// Run a write on a blocking task and render the item it returns.
async fn write_item<F>(state: AppState, f: F) -> ApiResult
where
    F: FnOnce(&AppState) -> Result<Item, ApiError> + Send + 'static,
{
    let data = blocking(move || {
        let item = f(&state)?;
        item_object(&state, &item)
    })
    .await?;
    Ok(ok(data, write_meta()))
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
    write_item(state, move |state| {
        let created = ops::create(
            &state.store,
            &state.id_prefix,
            state.default_type,
            spec,
            Utc::now(),
        )?;
        let id = parse_id(created["id"].as_str().unwrap_or_default())?;
        Ok(state.store.get(&id)?)
    })
    .await
}

// `deny_unknown_fields`: a key this endpoint does not edit (`parent`, `deps`,
// a typo) is an error, as it is for `clove set` and the MCP `clove_edit` tool,
// rather than a 200 that changed nothing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<u8>,
    // `double_option` makes an explicit `null` mean clear and an absent key mean
    // leave — a plain `Option<Option<_>>` would collapse `null` to "leave".
    #[serde(default, deserialize_with = "double_option")]
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

    // The same answer the MCP tool and `clove set` give for an edit that names
    // no field.
    if req.is_empty() {
        return Err(ApiError::from(CloveError::InvalidField {
            field: "body".to_owned(),
            reason: "no fields to edit".to_owned(),
        }));
    }

    write_item(state, move |state| {
        apply_edit(&state.store, &id, &req, Utc::now())?;
        Ok(state.store.get(&id)?)
    })
    .await
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
    write_item(state, move |state| {
        apply_edit(&state.store, &id, &req, Utc::now())?;
        Ok(state.store.get(&id)?)
    })
    .await
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
    let body = body.0;
    write_item(state, move |state| {
        if !state.store.exists(&id) {
            return Err(ApiError::from(CloveError::NotFound { id: id.to_string() }));
        }
        let author = body
            .author
            .filter(|a| !a.trim().is_empty())
            .unwrap_or_else(|| default_comment_author(state));
        core_add_comment(&state.issues_dir, &id, &author, &body.body)?;
        Ok(state.store.get(&id)?)
    })
    .await
}

/// The author of a comment whose request names none.
///
/// The shared rule (`CLOVE_AUTHOR`, the git identity, `$USER`) that
/// `clove comment` and the MCP `clove_comment` tool use, so a comment typed in
/// the browser is attributed to the same person as one typed in the terminal.
/// The server process may have nothing to resolve (the `cloved` hub runs with
/// a minimal environment); it then keeps the surface name `web` rather than
/// the rule's `unknown`.
fn default_comment_author(state: &AppState) -> String {
    let author = clove_core::comments::comment_author(state.store.repo_root());
    if author == "unknown" {
        "web".to_owned()
    } else {
        author
    }
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
    write_item(state, move |state| {
        ops::dep_add(&state.store, &id, &dep, Utc::now())?;
        Ok(state.store.get(&id)?)
    })
    .await
}

/// `DELETE /api/v1/items/:id/deps/:dep`. Idempotent (HTTP DELETE semantics): a
/// no-op if the dependency isn't present.
pub async fn remove_dep(
    State(state): State<AppState>,
    Path((id, dep)): Path<(String, String)>,
) -> ApiResult {
    let id = parse_id(&id)?;
    let dep = parse_id(&dep)?;
    write_item(state, move |state| {
        if state.store.get(&id)?.frontmatter.deps.contains(&dep) {
            ops::dep_remove(&state.store, &id, &dep, Utc::now())?;
        }
        Ok(state.store.get(&id)?)
    })
    .await
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
    write_item(state, move |state| {
        ops::set_parent(&state.store, &id, parent.as_ref(), Utc::now())?;
        Ok(state.store.get(&id)?)
    })
    .await
}

/// `DELETE /api/v1/items/:id?force=`.
pub async fn delete_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let id = parse_id(&id)?;
    // Through `bool_param`, like every other flag: `?force=1` used to read as
    // `false`, and `?force=yes` is now an error rather than a refused delete.
    let force = bool_param(&params, "force")?;
    let deleted = id.to_string();
    blocking(move || Ok(state.store.delete(&id, force)?)).await?;
    Ok(ok(json!({ "id": deleted, "deleted": true }), write_meta()))
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
            // `body_text`, not `Display`: it carries the cause (which key,
            // which line), where `Display` only names the rejection class.
            .map_err(|e| {
                ApiError::bad_request(format!("invalid request body: {}", e.body_text()))
            })?;
        Ok(ApiJson(value))
    }
}
