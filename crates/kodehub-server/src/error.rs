use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use kodehub_runner::RunnerError;
use kodehub_store::StoreError;
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub kind: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(m: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::BAD_REQUEST,
            kind: "invalid",
            message: m.into(),
        }
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::NOT_FOUND,
            kind: "not_found",
            message: m.into(),
        }
    }
    pub fn conflict(m: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::CONFLICT,
            kind: "conflict",
            message: m.into(),
        }
    }
    pub fn internal(m: impl Into<String>) -> Self {
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            kind: "internal",
            message: m.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": self.message, "kind": self.kind})),
        )
            .into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        let (status, kind) = match &e {
            StoreError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            StoreError::Arango { num: 1202, .. } => (StatusCode::NOT_FOUND, "not_found"),
            StoreError::Invalid(_) | StoreError::Seed(_) | StoreError::Graph(_) => {
                (StatusCode::BAD_REQUEST, "invalid")
            }
            StoreError::Immutable(_) => (StatusCode::CONFLICT, "immutable"),
            StoreError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            StoreError::Invariant(_) => (StatusCode::CONFLICT, "invariant"),
            StoreError::Arango { num: 1210, .. } => (StatusCode::CONFLICT, "conflict"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        ApiError {
            status,
            kind,
            message: e.to_string(),
        }
    }
}

impl From<RunnerError> for ApiError {
    fn from(e: RunnerError) -> Self {
        match e {
            RunnerError::Store(s) => s.into(),
            RunnerError::Render(r) => ApiError {
                status: StatusCode::BAD_REQUEST,
                kind: "not_runnable",
                message: r.to_string(),
            },
            RunnerError::Invalid(m) => ApiError::bad_request(m),
            RunnerError::Graph(g) => ApiError::bad_request(g.to_string()),
            other => ApiError::internal(other.to_string()),
        }
    }
}

impl From<kodehub_core::GraphError> for ApiError {
    fn from(e: kodehub_core::GraphError) -> Self {
        ApiError::bad_request(e.to_string())
    }
}

pub type ApiResult<T> = Result<Json<T>, ApiError>;
