//! Releases and environments (doc 06) — promotion goes through the environment agents' gates.

use axum::extract::{Path, State};
use axum::Json;
use kodehub_agents::environments::EnvironmentAgent;
use kodehub_store::releases::{code_swap, Selected, SelectionRule};
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
    /// Selection rule (spec §16.2); default: every stable version.
    #[serde(default)]
    pub rule: Option<SelectionRule>,
}

fn slim(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("graph");
        o.remove("trace");
        o.remove("root_closures");
    }
    v
}

fn agent(s: &AppState) -> EnvironmentAgent<'_> {
    EnvironmentAgent {
        store: &s.store,
        runner: &s.runner,
    }
}

/// Create a release from a selection rule; it is open and enters the lowest environment.
pub async fn cut(State(s): State<AppState>, body: Option<Json<CutBody>>) -> ApiResult<Value> {
    let b = body.map(|b| b.0).unwrap_or_default();
    let rel = agent(&s)
        .create(
            b.name.as_deref(),
            &b.notes,
            &b.rule.unwrap_or_default(),
            b.by.as_deref().unwrap_or("human"),
        )
        .await
        .map_err(agent_err)?;
    s.events.emit(
        "env.deployed",
        json!({"environment": rel["environment"], "release": rel["_key"], "created": true}),
    );
    Ok(Json(slim(rel)))
}

#[derive(Deserialize, Default)]
pub struct ReselectBody {
    #[serde(default)]
    pub rule: Option<SelectionRule>,
}

pub async fn reselect(
    State(s): State<AppState>,
    Path(k): Path<String>,
    body: Option<Json<ReselectBody>>,
) -> ApiResult<Value> {
    let b = body.map(|b| b.0).unwrap_or_default();
    Ok(Json(slim(
        s.store
            .reselect_release(&k, b.rule.as_ref(), "human")
            .await?,
    )))
}

pub async fn selection(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    let sel = s.store.release_selection(&k).await?;
    Ok(Json(json!(sel
        .iter()
        .map(|x| json!({"node": x.node, "collection": x.coll, "version": x.version}))
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
pub struct CloneBody {
    pub environment: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Clone into the release's own environment or a lower one (spec §16.2).
pub async fn clone(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(b): Json<CloneBody>,
) -> ApiResult<Value> {
    let v = s
        .store
        .clone_release(&k, &b.environment, &[], b.name.as_deref(), "human")
        .await?;
    s.events.emit(
        "env.deployed",
        json!({"release": v["_key"], "cloned_from": k}),
    );
    Ok(Json(slim(v)))
}

#[derive(Deserialize)]
pub struct SwapItem {
    /// `symbols/x` (or a bare symbol key), or a C4 node id.
    #[serde(alias = "symbol")]
    pub node: String,
    pub version: String,
}

#[derive(Deserialize)]
pub struct PatchBody {
    pub swaps: Vec<SwapItem>,
    #[serde(default)]
    pub approval: Option<String>,
}

fn to_selected(items: &[SwapItem]) -> Vec<Selected> {
    items
        .iter()
        .map(|i| {
            if i.node.contains('/') && !i.node.starts_with("symbols/") {
                Selected {
                    node: i.node.clone(),
                    coll: "c4_versions".into(),
                    version: i.version.clone(),
                }
            } else {
                code_swap(&i.node, &i.version)
            }
        })
        .collect()
}

/// Fix a bug found after DEV (spec §16.2): a patch release at the same environment.
pub async fn patch(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(b): Json<PatchBody>,
) -> ApiResult<Value> {
    let approved = redeem(&s, b.approval.as_deref(), &k);
    let v = agent(&s)
        .patch(&k, &to_selected(&b.swaps), approved, "human")
        .await
        .map_err(agent_err)?;
    s.events.emit(
        "env.deployed",
        json!({"release": v["release"], "patch_of": k, "patched": v["patched"]}),
    );
    Ok(Json(v))
}

/// Swap versions inside an open (DEV) release.
pub async fn swap(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(b): Json<PatchBody>,
) -> ApiResult<Value> {
    Ok(Json(slim(
        s.store
            .swap_release_versions(&k, &to_selected(&b.swaps), "human")
            .await?,
    )))
}

/// Redeem a one-time human approval (issued only to a signed-in human) for `release`.
fn redeem(s: &AppState, id: Option<&str>, release: &str) -> bool {
    let Some(id) = id else { return false };
    let release = kodehub_store::key_of(release).to_string();
    let mut m = s.approvals.lock().unwrap();
    match m.remove(id) {
        Some((rel, issued)) => {
            rel == release && issued.elapsed() < std::time::Duration::from_secs(600)
        }
        None => false,
    }
}

pub async fn get(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    let mut r = slim(s.store.get_release(&k).await?);
    r["environment"] = json!(s.store.release_environment(&k).await?);
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
    let a = agent(&s);
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
    let a = agent(&s);
    s.events.emit("agent.activity", json!({"role": format!("env:{env}"), "message": format!("checking gate for {}", b.release)}));
    let release = kodehub_store::key_of(&b.release).to_string();
    // redeem a one-time human approval (issued only to a human session) for this release
    let approved = redeem(&s, b.approval.as_deref(), &release);
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

#[derive(Deserialize, Default)]
pub struct RollbackBody {
    #[serde(default)]
    pub approval: Option<String>,
}

/// Roll back: the most recently retired release re-enters `env` through the same gate.
pub async fn rollback(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(env): Path<String>,
    body: Option<Json<RollbackBody>>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let target = s.store.rollback_target(&env).await?.unwrap_or_default();
    let approved = redeem(&s, body.and_then(|b| b.0.approval).as_deref(), &target);
    let v = agent(&s)
        .rollback(&env, approved, "human")
        .await
        .map_err(agent_err)?;
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
    let needs = s
        .store
        .env_defs()
        .await?
        .iter()
        .any(|e| e.name == env && e.requires_human_approval);
    if !needs {
        return Err(ApiError::bad_request(format!(
            "{env} does not need a human approval"
        )));
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
