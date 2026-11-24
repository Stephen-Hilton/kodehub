//! Work queue, schedules, triggers, intents and spend (spec §3.3, §4.4, §5.7, §10, §12.2).

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use kodehub_store::queue::NewWorkItem;
use kodehub_store::Principal;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::ProjectCtx;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

#[derive(Deserialize, Default)]
pub struct ListQ {
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn list(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Query(q): Query<ListQ>,
) -> ApiResult<Value> {
    Ok(Json(json!(
        s.store
            .list_work_items(Some(&p.id), q.state.as_deref(), q.limit.unwrap_or(200))
            .await?
    )))
}

pub async fn stats(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
) -> ApiResult<Value> {
    let mut v = s.store.queue_stats(&p.id).await?;
    v["spend"] = s.store.project_spend(&p.id).await?;
    v["in_progress"] = json!(s.store.in_progress_by_node(&p.id).await?);
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct NewItem {
    pub agent_type: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub task: Value,
}

/// Queue work by hand (humans and their external agents; containerized agents go through the
/// orchestrator).
pub async fn create(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Extension(who): Extension<Principal>,
    Json(b): Json<NewItem>,
) -> ApiResult<Value> {
    let w = s
        .store
        .enqueue(&NewWorkItem {
            project: p.id.clone(),
            agent_type: b.agent_type,
            kind: b.kind,
            target: b.target,
            priority: b.priority.unwrap_or(kodehub_store::queue::PRIORITY_NORMAL),
            task: b.task,
            requested_by: Some(who.id()),
            ..Default::default()
        })
        .await?;
    s.events.emit(
        "work.queued",
        json!({"key": w["_key"], "agent_type": w["agent_type"]}),
    );
    Ok(Json(w))
}

pub async fn get(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Path(k): Path<String>,
) -> ApiResult<Value> {
    let w = s.store.work_item(&k).await?;
    if w["project"] != json!(p.id) {
        return Err(ApiError::not_found(format!("work item {k}")));
    }
    Ok(Json(w))
}

pub async fn cancel(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Extension(who): Extension<Principal>,
    Path(k): Path<String>,
) -> ApiResult<Value> {
    let _owned = get(State(s.clone()), Extension(p), Path(k.clone())).await?;
    Ok(Json(s.store.cancel_work_item(&k, &who.id()).await?))
}

// ---------------------------------------------------------------------- schedules

pub async fn schedules(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
) -> ApiResult<Value> {
    Ok(Json(json!(s.store.list_schedules(&p.id).await?)))
}

#[derive(Deserialize)]
pub struct NewSchedule {
    pub target: String,
    #[serde(default = "node")]
    pub level: String,
    pub interval_secs: i64,
}

fn node() -> String {
    "node".into()
}

pub async fn create_schedule(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Json(b): Json<NewSchedule>,
) -> ApiResult<Value> {
    Ok(Json(
        s.store
            .upsert_schedule(&p.id, &b.target, &b.level, b.interval_secs, "manual")
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct Enabled {
    pub enabled: bool,
}

pub async fn schedule_enabled(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Path(k): Path<String>,
    Json(b): Json<Enabled>,
) -> ApiResult<Value> {
    Ok(Json(
        s.store.set_schedule_enabled(&p.id, &k, b.enabled).await?,
    ))
}

/// Rebuild the default cadence from settings and node lifecycles now.
pub async fn replan(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
) -> ApiResult<Value> {
    Ok(Json(json!(s.store.ensure_default_schedules(&p.id).await?)))
}

#[derive(Deserialize)]
pub struct TestNow {
    /// A node id, or `project` for the whole project.
    pub target: String,
}

/// The *Test now* button (spec §3.3): an event trigger the scheduler fires at once.
pub async fn test_now(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Extension(who): Extension<Principal>,
    Json(b): Json<TestNow>,
) -> ApiResult<Value> {
    if b.target != "project" {
        s.store.get_node(&b.target).await?;
    }
    let t = s
        .store
        .trigger(&p.id, "test_now", &b.target, &who.id())
        .await?;
    s.events.emit(
        "test.requested",
        json!({"target": b.target, "trigger": t["_key"]}),
    );
    Ok(Json(t))
}

pub async fn intents(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
) -> ApiResult<Value> {
    Ok(Json(json!({
        "intents": s.store.list_intents(&p.id).await?,
        "negotiations": s.store.list_negotiations(&p.id).await?,
    })))
}

pub async fn spend(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
) -> ApiResult<Value> {
    Ok(Json(s.store.project_spend(&p.id).await?))
}
