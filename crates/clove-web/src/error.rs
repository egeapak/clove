//! The HTTP error type and JSON envelope helpers.
//!
//! Mirrors the CLI's JSON envelope (DESIGN §7.3) and exit-code classification
//! (§7.6) so the web API and the CLI return the same `error.code`/`error.exit`
//! for the same failure — only the transport (HTTP status) is added.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use clove_types::CloveError;
use serde_json::{json, Value};

/// A web API error: the stable string code + numeric exit code from the CLI
/// contract, plus the HTTP status to send.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub exit: u8,
    pub message: String,
}

impl ApiError {
    /// A bad-request (usage-class) error for malformed input the CLI would
    /// reject before reaching core (bad query param, missing body field, …).
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "USAGE_ERROR",
            exit: 1,
            message: message.into(),
        }
    }
}

/// Map a [`CloveError`] to an [`ApiError`]. The `(code, exit)` pair comes from
/// the shared [`clove_types::error_code`] classifier (so the web API and CLI agree
/// on `error.code`/`error.exit`); only the HTTP status is web-specific.
impl From<CloveError> for ApiError {
    fn from(error: CloveError) -> Self {
        let (code, exit) = clove_types::error_code(&error);
        let status = http_status(&error, exit);
        Self {
            status,
            code,
            exit,
            message: error.to_string(),
        }
    }
}

/// The HTTP status for a `CloveError`: derived from the shared `exit` class, with
/// a few variant-specific refinements (a conflict is a 409, validation a 422).
fn http_status(error: &CloveError, exit: u8) -> StatusCode {
    match error {
        // Resource conflicts are 409 even though their exit class is 4.
        CloveError::DependencyExists { .. }
        | CloveError::HasDependents { .. }
        | CloveError::IdConflict { .. }
        | CloveError::CommentConflict { .. } => StatusCode::CONFLICT,
        _ => match exit {
            1 => StatusCode::BAD_REQUEST,
            2 => StatusCode::NOT_FOUND,
            3 => StatusCode::CONFLICT,
            4 => StatusCode::UNPROCESSABLE_ENTITY,
            7 => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        },
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({
            "v": 1,
            "ok": false,
            "error": { "code": self.code, "message": self.message, "exit": self.exit },
        });
        (self.status, Json(body)).into_response()
    }
}

/// The success envelope `{ v, ok:true, data, _meta }` as an axum response.
pub fn ok(data: Value, meta: Value) -> Response {
    Json(json!({ "v": 1, "ok": true, "data": data, "_meta": meta })).into_response()
}

/// The success envelope with an empty `_meta`.
pub fn ok_data(data: Value) -> Response {
    ok(data, json!({}))
}

/// A web API result.
pub type ApiResult = Result<Response, ApiError>;
