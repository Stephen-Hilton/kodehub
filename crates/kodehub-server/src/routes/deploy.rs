//! Project versions, environments and deployments (D-059, D-060; doc 06). A project version is
//! the release; deploying it into an environment goes through the environment agent's gates.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use kodehub_agents::environments::EnvironmentAgent;
use kodehub_store::Principal;
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

fn agent(s: &AppState) -> EnvironmentAgent<'_> {
    EnvironmentAgent {
        store: &s.store,
        runner: &s.runner,
    }
}

// ---------------------------------------------------------------------- project versions

/// Every project version, newest first, with where it runs.
pub async fn list_versions(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    Ok(Json(s.store.project_versions_overview().await?))
}

pub async fn get_version(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    Ok(Json(s.store.get_project_version(&k).await?))
}

#[derive(Deserialize, Default)]
pub struct NewVersionBody {
    #[serde(default)]
    pub name: Option<String>,
    /// The version it starts from (default: the current one).
    #[serde(default)]
    pub based_on: Option<String>,
    /// Make it the project's current version (default true).
    #[serde(default)]
    pub make_current: Option<bool>,
}

/// Open a new project version (the next release), starting from another's tree.
pub async fn create_version(
    State(s): State<AppState>,
    Extension(who): Extension<Principal>,
    body: Option<Json<NewVersionBody>>,
) -> ApiResult<Value> {
    let b = body.map(|b| b.0).unwrap_or_default();
    let base = match b.based_on {
        Some(x) => s.store.pv_id(&x)?,
        None => s.store.current_version().await?,
    };
    let v = s
        .store
        .create_project_version(b.name.as_deref(), Some(&base), &who.id())
        .await?;
    if b.make_current.unwrap_or(true) {
        s.store
            .set_current_version(v["_id"].as_str().unwrap_or_default())
            .await?;
    }
    s.events
        .emit("project_version.changed", json!({"id": v["_id"]}));
    Ok(Json(
        s.store
            .get_project_version(v["_id"].as_str().unwrap_or_default())
            .await?,
    ))
}

#[derive(Deserialize, Default)]
pub struct FreezeBody {
    #[serde(default)]
    pub reason: Option<String>,
}

/// Freeze a project version (a milestone): it never changes again.
pub async fn freeze_version(
    State(s): State<AppState>,
    Extension(who): Extension<Principal>,
    Path(k): Path<String>,
    body: Option<Json<FreezeBody>>,
) -> ApiResult<Value> {
    let reason = body
        .and_then(|b| b.0.reason)
        .unwrap_or_else(|| "frozen by hand".into());
    s.store
        .freeze_project_version(&k, &who.id(), &reason)
        .await?;
    s.events.emit(
        "project_version.changed",
        json!({"id": k, "change": "frozen"}),
    );
    Ok(Json(s.store.get_project_version(&k).await?))
}

/// Make an open project version the default for new work.
pub async fn make_current(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    s.store.set_current_version(&k).await?;
    s.events.emit(
        "project_version.changed",
        json!({"id": k, "change": "current"}),
    );
    Ok(Json(s.store.get_project_version(&k).await?))
}

#[derive(Deserialize, Default)]
pub struct HotfixBody {
    #[serde(default)]
    pub name: Option<String>,
}

/// A hotfix of a frozen project version: a new open version starting from its tree.
pub async fn hotfix(
    State(s): State<AppState>,
    Extension(who): Extension<Principal>,
    Path(k): Path<String>,
    body: Option<Json<HotfixBody>>,
) -> ApiResult<Value> {
    let b = body.map(|b| b.0).unwrap_or_default();
    let v = s
        .store
        .create_hotfix(&k, b.name.as_deref(), &who.id())
        .await?;
    s.events.emit(
        "project_version.changed",
        json!({"id": v["_id"], "hotfix_of": v["hotfix_of"]}),
    );
    Ok(Json(
        s.store
            .get_project_version(v["_id"].as_str().unwrap_or_default())
            .await?,
    ))
}

pub async fn diff(
    State(s): State<AppState>,
    Path((a, b)): Path<(String, String)>,
) -> ApiResult<Value> {
    Ok(Json(s.store.diff_project_versions(&a, &b).await?))
}

pub async fn trace(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    Ok(Json(s.store.project_version_trace(&k).await?))
}

#[derive(Deserialize)]
pub struct BisectBody {
    pub good: String,
    pub bad: String,
    #[serde(default)]
    pub test: Option<String>,
}

pub async fn bisect(State(s): State<AppState>, Json(b): Json<BisectBody>) -> ApiResult<Value> {
    Ok(Json(
        agent(&s)
            .bisect(&b.good, &b.bad, b.test.as_deref())
            .await
            .map_err(agent_err)?,
    ))
}

// ---------------------------------------------------------------------- environments

pub async fn environments(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    Ok(Json(s.store.environments().await?))
}

// ---------------------------------------------------------------------- deployments

#[derive(Deserialize)]
pub struct DeploymentsQuery {
    #[serde(default)]
    pub environment: Option<String>,
}

pub async fn list_deployments(
    State(s): State<AppState>,
    Query(q): Query<DeploymentsQuery>,
) -> ApiResult<Vec<Value>> {
    Ok(Json(
        s.store.list_deployments(q.environment.as_deref()).await?,
    ))
}

pub async fn get_deployment(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    Ok(Json(s.store.get_deployment(&k).await?))
}

#[derive(Deserialize)]
pub struct DeployBody {
    pub project_version: String,
    pub environment: String,
    /// `promote` (default; ordered: one environment up at a time) or `hotfix` (straight into
    /// the environment it fixes).
    #[serde(default)]
    pub reason: Option<String>,
}

/// Deploy a project version into an environment (gated). Production waits for a human.
pub async fn deploy(
    State(s): State<AppState>,
    Extension(who): Extension<Principal>,
    Json(b): Json<DeployBody>,
) -> ApiResult<Value> {
    let reason = b.reason.unwrap_or_else(|| "promote".into());
    let ordered = reason == "promote";
    let v = agent(&s)
        .deploy(
            &b.project_version,
            &b.environment,
            &who.id(),
            &reason,
            ordered,
        )
        .await
        .map_err(agent_err)?;
    s.events.emit(
        "env.deployed",
        json!({"environment": b.environment, "project_version": b.project_version,
               "deployed": v["deployed"]}),
    );
    Ok(Json(v))
}

/// A human approves a production deployment; it then runs.
pub async fn approve(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Extension(who): Extension<Principal>,
    Path(k): Path<String>,
) -> ApiResult<Value> {
    if !crate::from_ui(&s, &headers) {
        return Err(ApiError {
            status: axum::http::StatusCode::FORBIDDEN,
            kind: "human_only",
            message: "production deployments are approved by a human in the app".into(),
        });
    }
    let v = agent(&s)
        .approve_and_run(&k, &who.id())
        .await
        .map_err(agent_err)?;
    s.events.emit(
        "env.deployed",
        json!({"deployment": k, "deployed": v["deployed"]}),
    );
    Ok(Json(v))
}

/// Retry a failed deployment (a cloud hiccup): a new attempt, logged like the first.
pub async fn retry(
    State(s): State<AppState>,
    Extension(who): Extension<Principal>,
    Path(k): Path<String>,
) -> ApiResult<Value> {
    let v = agent(&s).run(&k, &who.id()).await.map_err(agent_err)?;
    s.events.emit(
        "env.deployed",
        json!({"deployment": k, "deployed": v["deployed"]}),
    );
    Ok(Json(v))
}

/// Roll an environment back to the project version it ran before (gated).
pub async fn rollback(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Extension(who): Extension<Principal>,
    Path(env): Path<String>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let v = agent(&s)
        .rollback(&env, &who.id())
        .await
        .map_err(agent_err)?;
    s.events.emit(
        "env.deployed",
        json!({"environment": env, "rollback": true, "project_version": v["rolled_back_to"]}),
    );
    Ok(Json(v))
}
