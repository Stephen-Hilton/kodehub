//! Agent team status and the orchestrator chat.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub async fn status(State(s): State<AppState>) -> ApiResult<Value> {
    let cfg = s.config.read().await.clone();
    let mut v = s
        .engine
        .as_ref()
        .map(|e| e.agent_status())
        .unwrap_or(json!({"agents": []}));
    v["backend"] = json!(cfg.agents.backend);
    v["token_env"] = json!(cfg.agents.token_env);
    v["model"] = json!(cfg.agents.model);
    let open = s.store.list_changesets(true).await?;
    v["open_changesets"] = json!(open.len());
    Ok(Json(v))
}

pub async fn history(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    Ok(Json(
        s.store
            .aql(
                "FOR m IN @@chat_messages SORT m.created_at DESC LIMIT 100 RETURN m",
                json!({}),
            )
            .await?
            .into_iter()
            .rev()
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct ChatBody {
    pub message: String,
    #[serde(default)]
    pub references: Vec<String>,
}

pub async fn chat(State(s): State<AppState>, Json(b): Json<ChatBody>) -> ApiResult<Value> {
    if b.message.trim().is_empty() {
        return Err(ApiError::bad_request("message is empty"));
    }
    let e = s
        .engine
        .clone()
        .ok_or_else(|| ApiError::bad_request("agents are not configured"))?;
    match e.chat(&b.message, &b.references).await {
        Ok(v) => Ok(Json(v)),
        Err(err) => Err(ApiError {
            status: axum::http::StatusCode::BAD_GATEWAY,
            kind: "agent",
            message: err.to_string(),
        }),
    }
}

#[derive(Deserialize)]
pub struct ApplyBody {
    pub apply: bool,
}

/// Apply or discard an action the orchestrator proposed in chat (human click only).
pub async fn apply_chat(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(k): axum::extract::Path<String>,
    Json(b): Json<ApplyBody>,
) -> ApiResult<Value> {
    if !crate::from_ui(&s, &headers) {
        return Err(ApiError {
            status: axum::http::StatusCode::FORBIDDEN,
            kind: "human_only",
            message: "chat proposals are applied by a human in the app".into(),
        });
    }
    let e = s
        .engine
        .clone()
        .ok_or_else(|| ApiError::bad_request("agents are not configured"))?;
    match e.apply_chat_action(&k, b.apply).await {
        Ok(v) => {
            s.events
                .emit("node.changed", json!({"change": "chat_action"}));
            Ok(Json(v))
        }
        Err(err) => Err(ApiError::bad_request(err.to_string())),
    }
}
