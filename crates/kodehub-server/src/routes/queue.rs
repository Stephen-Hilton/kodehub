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

// ---------------------------------------------------------------------- git import / export

#[derive(Deserialize)]
pub struct GitImport {
    pub repo_url: String,
    #[serde(default)]
    pub r#ref: Option<String>,
}

/// One-time import of a repository into this project (spec §13.1). Re-importing the same
/// repository would be a reverse sync, which is out of scope.
pub async fn git_import(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Extension(who): Extension<Principal>,
    Json(b): Json<GitImport>,
) -> ApiResult<Value> {
    let url = b.repo_url.trim().to_string();
    if !(url.starts_with("https://") || url.starts_with("file://") || url.starts_with("git@")) {
        return Err(ApiError::bad_request(
            "repo_url must be https://, git@ or file://",
        ));
    }
    let earlier = s
        .store
        .list_work_items(Some(&p.id), None, 2000)
        .await?
        .into_iter()
        .any(|w| {
            w["agent_type"] == "git_import"
                && w["task"]["repo_url"] == json!(url)
                && w["state"] != "failed"
        });
    if earlier {
        return Err(ApiError::conflict(
            "this repository was already imported; re-import (reverse sync) is out of scope",
        ));
    }
    let w = s
        .store
        .enqueue(&NewWorkItem {
            project: p.id.clone(),
            agent_type: "git_import".into(),
            kind: "import".into(),
            target: Some("philosophy/philosophy".into()),
            priority: kodehub_store::queue::PRIORITY_NORMAL,
            task: json!({"repo_url": url, "ref": b.r#ref}),
            requested_by: Some(who.id()),
            ..Default::default()
        })
        .await?;
    Ok(Json(w))
}

#[derive(Deserialize)]
pub struct GitExport {
    pub repo_url: String,
    #[serde(default = "main_branch")]
    pub branch: String,
    #[serde(default)]
    pub message: Option<String>,
}

fn main_branch() -> String {
    "main".into()
}

/// Mirror the project's live versions to a Git repository (spec §13.2).
pub async fn git_export(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Extension(who): Extension<Principal>,
    Json(b): Json<GitExport>,
) -> ApiResult<Value> {
    let w = s
        .store
        .enqueue(&NewWorkItem {
            project: p.id.clone(),
            agent_type: "git_export".into(),
            kind: "export".into(),
            target: Some("philosophy/philosophy".into()),
            priority: kodehub_store::queue::PRIORITY_NORMAL,
            task: json!({"repo_url": b.repo_url, "branch": b.branch,
                         "message": b.message.unwrap_or_else(|| format!("Kodehub export of {}", p.id))}),
            requested_by: Some(who.id()),
            ..Default::default()
        })
        .await?;
    Ok(Json(w))
}

// ---------------------------------------------------------------------- database backups

/// Back up this project's graph into storage (hybrid mode ships it off-site).
pub async fn backup(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
) -> ApiResult<Value> {
    let dump = s.store.export_project(&p.id).await?;
    let storage = kodehub_storage::StorageClient::from_env_or_local(&s.runner.home);
    let key = format!("projects/{}/backups/db-{}.json", p.id, chrono_like_now());
    let bytes = serde_json::to_vec(&dump).map_err(|e| ApiError::internal(e.to_string()))?;
    storage
        .put(kodehub_storage::BUCKET, &key, bytes.clone())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(json!({"key": key, "bytes": bytes.len()})))
}

fn chrono_like_now() -> String {
    kodehub_core::model::now_iso().replace([':', '.'], "-")
}

#[derive(Deserialize)]
pub struct Restore {
    pub key: String,
    pub name: String,
}

/// Restore one project alone from a backup, as a new project owned by the caller.
pub async fn restore(
    State(s): State<AppState>,
    Extension(who): Extension<Principal>,
    Json(b): Json<Restore>,
) -> ApiResult<Value> {
    if !who.is_human() {
        return Err(ApiError::bad_request(
            "restores are started by a signed-in human",
        ));
    }
    let source = b.key.split('/').nth(1).unwrap_or_default().to_string();
    if s.store.project_permission(&who, &source).await? != Some(kodehub_store::Permission::Admin) {
        return Err(ApiError::bad_request(
            "restore needs admin access to the backed-up project",
        ));
    }
    let storage = kodehub_storage::StorageClient::from_env_or_local(&s.runner.home);
    let data = storage
        .get(kodehub_storage::BUCKET, &b.key)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("backup {}", b.key)))?;
    let dump: Value =
        serde_json::from_slice(&data).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let out = s
        .store
        .restore_project(
            &dump,
            &b.name,
            &kodehub_store::Owner::User(who.user.clone()),
        )
        .await?;
    Ok(Json(out))
}

// ---------------------------------------------------------------------- speech transcription

/// Speech to text for the philosophy document and requirements (spec §14.1): the audio goes to
/// storage, a transcription container runs a local Whisper model, the text comes back. No
/// browser speech service is used, so Chrome and Tauri's WebKit behave the same.
pub async fn transcribe(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Value> {
    if body.is_empty() {
        return Err(ApiError::bad_request("no audio"));
    }
    if body.len() > 25 * 1024 * 1024 {
        return Err(ApiError::bad_request("audio over 25 MB"));
    }
    let ct = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/webm");
    let ext = match ct.split(';').next().unwrap_or("") {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/ogg" => "ogg",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/mpeg" => "mp3",
        _ => "webm",
    };
    let storage = kodehub_storage::StorageClient::from_env_or_local(&s.runner.home);
    let id = uuid::Uuid::new_v4().simple().to_string();
    let audio_key = format!("projects/{}/audio/{id}.{ext}", p.id);
    let in_key = format!("projects/{}/jobs/transcribe-{id}/input.json", p.id);
    let out_key = format!("projects/{}/jobs/transcribe-{id}/output.json", p.id);
    let err = |e: anyhow::Error| ApiError::internal(e.to_string());
    storage
        .put(kodehub_storage::BUCKET, &audio_key, body)
        .await
        .map_err(err)?;
    storage
        .put(
            kodehub_storage::BUCKET,
            &in_key,
            serde_json::to_vec(&json!({"audio_key": audio_key, "ext": ext})).unwrap_or_default(),
        )
        .await
        .map_err(err)?;
    let outcome = kodehub_runner::jobs::JobRuntime::from_env()
        .run(&kodehub_runner::jobs::JobSpec {
            kind: "transcribe".into(),
            container_type: None,
            input_key: in_key,
            output_key: out_key.clone(),
            timeout: std::time::Duration::from_secs(300),
            env: vec![],
            secret_env: vec![],
            name: format!("transcribe-{}", &id[..8]),
        })
        .await?;
    let out: Value = match storage
        .get(kodehub_storage::BUCKET, &out_key)
        .await
        .map_err(err)?
    {
        Some(b) => serde_json::from_slice(&b).unwrap_or(Value::Null),
        None => {
            return Err(ApiError::internal(format!(
                "transcription wrote no output: {}",
                outcome.logs
            )))
        }
    };
    if out["ok"] != true {
        return Err(ApiError::internal(
            out["error"]
                .as_str()
                .unwrap_or("transcription failed")
                .to_string(),
        ));
    }
    Ok(Json(json!({"text": out["text"]})))
}
