//! Releases and environments (doc 06) — promotion goes through the environment agents' gates.

use axum::extract::{Path, State};
use axum::Json;
use kodehub_agents::environments::EnvironmentAgent;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

fn agent_err(e: kodehub_agents::AgentError) -> ApiError {
    match e {
        kodehub_agents::AgentError::Store(s) => s.into(),
        kodehub_agents::AgentError::Runner(r) => r.into(),
        other => ApiError::bad_request(other.to_string()),
    }
}

pub async fn list(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    Ok(Json(s.store.list_releases().await?))
}

#[derive(Deserialize, Default)]
pub struct CutBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub by: Option<String>,
}

pub async fn cut(State(s): State<AppState>, body: Option<Json<CutBody>>) -> ApiResult<Value> {
    let b = body.map(|b| b.0).unwrap_or_default();
    let a = EnvironmentAgent {
        store: &s.store,
        runner: &s.runner,
    };
    let rel = a
        .cut(
            b.name.as_deref(),
            &b.notes,
            b.by.as_deref().unwrap_or("human"),
        )
        .await
        .map_err(agent_err)?;
    s.events.emit(
        "env.deployed",
        json!({"environment": "dev", "release": rel["_key"], "cut": true}),
    );
    let mut v = rel.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("graph");
    }
    Ok(Json(v))
}

pub async fn get(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    let mut r = s.store.get_release(&k).await?;
    if let Some(o) = r.as_object_mut() {
        o.remove("graph");
        o.remove("trace");
    }
    Ok(Json(r))
}

pub async fn diff(
    State(s): State<AppState>,
    Path((a, b)): Path<(String, String)>,
) -> ApiResult<Value> {
    Ok(Json(s.store.diff_releases(&a, &b).await?))
}

#[derive(Deserialize)]
pub struct BisectBody {
    pub good: String,
    pub bad: String,
    #[serde(default)]
    pub test: Option<String>,
}

pub async fn bisect(State(s): State<AppState>, Json(b): Json<BisectBody>) -> ApiResult<Value> {
    let a = EnvironmentAgent {
        store: &s.store,
        runner: &s.runner,
    };
    Ok(Json(
        a.bisect(&b.good, &b.bad, b.test.as_deref())
            .await
            .map_err(agent_err)?,
    ))
}

pub async fn environments(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    Ok(Json(s.store.environments().await?))
}

#[derive(Deserialize)]
pub struct PromoteBody {
    pub release: String,
    /// One-time approval id from `POST /api/environments/prod/approvals` (prod only).
    #[serde(default)]
    pub approval: Option<String>,
    #[serde(default)]
    pub by: Option<String>,
}

pub async fn promote(
    State(s): State<AppState>,
    Path(env): Path<String>,
    Json(b): Json<PromoteBody>,
) -> ApiResult<Value> {
    let a = EnvironmentAgent {
        store: &s.store,
        runner: &s.runner,
    };
    s.events.emit("agent.activity", json!({"role": format!("env:{env}"), "message": format!("checking gate for {}", b.release)}));
    let release = kodehub_store::key_of(&b.release).to_string();
    // redeem a one-time human approval (issued only to the UI) for this release (review M6 #7)
    let approved = match &b.approval {
        Some(id) => {
            let mut m = s.approvals.lock().unwrap();
            match m.remove(id) {
                Some((rel, issued)) => {
                    rel == release && issued.elapsed() < std::time::Duration::from_secs(600)
                }
                None => false,
            }
        }
        None => false,
    };
    let v = a
        .promote(&release, &env, approved, b.by.as_deref().unwrap_or("human"))
        .await
        .map_err(agent_err)?;
    if v["promoted"] == json!(true) {
        s.events.emit(
            "env.deployed",
            json!({"environment": env, "release": b.release}),
        );
    } else {
        s.events.emit(
            "agent.activity",
            json!({"role": format!("env:{env}"), "message": "gate failed", "gate": v["gate"]}),
        );
    }
    Ok(Json(v))
}

pub async fn rollback(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(env): Path<String>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let v = s.store.rollback(&env, "human").await?;
    s.events.emit(
        "env.deployed",
        json!({"environment": env, "release": v["release"], "rollback": true}),
    );
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct ApprovalBody {
    pub release: String,
}

/// Human sign-off for a prod promotion: a one-time id, only issued to the UI.
pub async fn approve_prod(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(env): Path<String>,
    Json(b): Json<ApprovalBody>,
) -> ApiResult<Value> {
    if !crate::from_ui(&s, &headers) {
        return Err(ApiError {
            status: axum::http::StatusCode::FORBIDDEN,
            kind: "human_only",
            message: "prod approvals are given by a human in the app".into(),
        });
    }
    if env != "prod" {
        return Err(ApiError::bad_request("approvals are only needed for prod"));
    }
    let id = format!("appr-{}", uuid_like());
    s.approvals.lock().unwrap().insert(
        id.clone(),
        (
            kodehub_store::key_of(&b.release).to_string(),
            std::time::Instant::now(),
        ),
    );
    Ok(Json(
        json!({"approval": id, "release": b.release, "expires_in_secs": 600}),
    ))
}

fn uuid_like() -> String {
    // unguessable approval ids (final review #8)
    uuid::Uuid::new_v4().simple().to_string()
}
