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

// ---------------------------------------------------------------------- approvals & notifications

#[derive(Deserialize, Default)]
pub struct StateQ {
    pub state: Option<String>,
}

pub async fn approvals(State(s): State<AppState>, Query(q): Query<StateQ>) -> ApiResult<Value> {
    Ok(Json(json!(
        s.store.list_approvals(q.state.as_deref()).await?
    )))
}

#[derive(Deserialize)]
pub struct Decide {
    pub approve: bool,
    #[serde(default)]
    pub note: String,
}

/// Requirement changes and new contexts are a human's call (spec §4.5, §7.3).
pub async fn decide(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Extension(who): Extension<Principal>,
    Path(k): Path<String>,
    Json(b): Json<Decide>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let v = s
        .store
        .decide_approval(&k, b.approve, &who.id(), &b.note)
        .await?;
    s.events
        .emit("approval.decided", json!({"key": k, "approve": b.approve}));
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
pub struct NotifQ {
    #[serde(default)]
    pub unread: bool,
}

pub async fn notifications(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Query(q): Query<NotifQ>,
) -> ApiResult<Value> {
    Ok(Json(json!(
        s.store.notifications(Some(&p.id), q.unread).await?
    )))
}

pub async fn read_notification(
    State(s): State<AppState>,
    Path(k): Path<String>,
) -> ApiResult<Value> {
    s.store.mark_notification_read(&k).await?;
    Ok(Json(json!({"read": k})))
}

/// After a human cleared an external blocker (e.g. added a credential), requeue blocked work.
pub async fn unblock(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
) -> ApiResult<Value> {
    Ok(Json(
        json!({"requeued": s.store.unblock_work_items(&p.id).await?}),
    ))
}

// ---------------------------------------------------------------------- agent types

pub async fn agent_types(State(s): State<AppState>) -> ApiResult<Value> {
    kodehub_agents::roles::ensure_agent_types(&s.store)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(json!(
        s.store
            .aql("FOR t IN @@agent_types SORT t._key RETURN t", json!({}))
            .await?
    )))
}

#[derive(Deserialize)]
pub struct CustomAgent {
    pub instructions: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// A custom agent type (spec §4.1), e.g. a policy agent: the same base agent with a role
/// definition stored in the graph.
pub async fn put_agent_type(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(b): Json<CustomAgent>,
) -> ApiResult<Value> {
    let slug = kodehub_store::slug(name.trim_start_matches("custom:"));
    if slug.is_empty() || b.instructions.trim().is_empty() {
        return Err(ApiError::bad_request(
            "a custom agent needs a name and instructions",
        ));
    }
    let key = format!("custom-{slug}");
    s.store
        .db
        .upsert(
            "agent_types",
            &json!({"_key": key, "builtin": false, "prompt": "custom", "tool_role": "orchestrator",
                    "instructions": b.instructions, "description": b.description, "model": b.model}),
            None,
        )
        .await?;
    Ok(Json(s.store.db.get("agent_types", &key, None).await?))
}
