//! Philosophy, requirement cascade, use cases and lifecycle (spec §7, §14.4, §15).

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::{Extension, Json};
use kodehub_store::Principal;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub async fn get_philosophy(State(s): State<AppState>) -> ApiResult<Value> {
    Ok(Json(s.store.get_philosophy().await?.unwrap_or(Value::Null)))
}

#[derive(Deserialize)]
pub struct PhilosophyBody {
    pub text: String,
    #[serde(default)]
    pub title: Option<String>,
    /// A raw speech transcript; the architect agent cleans it up (spec §14.1).
    #[serde(default)]
    pub raw_transcript: Option<String>,
}

/// Humans (and their external agents, spec §14.1 MCP intake) set direction; containerized
/// agents may only clean up through a work item.
pub async fn put_philosophy(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(b): Json<PhilosophyBody>,
) -> ApiResult<Value> {
    if b.text.trim().is_empty() {
        return Err(ApiError::bad_request("the philosophy document is empty"));
    }
    let doc = s
        .store
        .set_philosophy_with(
            &b.text,
            &p.id(),
            b.title.as_deref(),
            b.raw_transcript.as_deref(),
        )
        .await?;
    s.events
        .emit("node.changed", json!({"id": s.store.philosophy_id()?}));
    Ok(Json(doc))
}

#[derive(Deserialize)]
pub struct NodeQ {
    pub node: String,
}

/// Requirements for a node, own and inherited, with the cascade chain (spec §7.2, §8.3).
pub async fn cascade(State(s): State<AppState>, Query(q): Query<NodeQ>) -> ApiResult<Value> {
    s.store.get_node(&q.node).await?;
    Ok(Json(s.store.requirements_for(&q.node).await?))
}

#[derive(Deserialize)]
pub struct ByQ {
    pub kind: Option<String>,
    pub level: Option<String>,
}

pub async fn requirements_by(State(s): State<AppState>, Query(q): Query<ByQ>) -> ApiResult<Value> {
    Ok(Json(json!(
        s.store
            .requirements_by(q.kind.as_deref(), q.level.as_deref())
            .await?
    )))
}

pub async fn push_down(State(s): State<AppState>) -> ApiResult<Value> {
    Ok(Json(json!(s.store.push_down_suggestions().await?)))
}

pub async fn lifecycle(State(s): State<AppState>) -> ApiResult<Value> {
    Ok(Json(json!(s.store.node_lifecycle().await?)))
}

pub async fn requirement_counts(State(s): State<AppState>) -> ApiResult<Value> {
    Ok(Json(json!(s.store.requirement_counts().await?)))
}

/// Every version of a node (C4, code or test), newest first: `accepted` (written into a
/// project version), `candidate` (a work item's proposal) or `discarded`; `current` marks the
/// one in the current project version, `project_versions` names every version holding it.
pub async fn versions(State(s): State<AppState>, Query(q): Query<NodeQ>) -> ApiResult<Value> {
    let node = s.store.db.qid(&q.node)?;
    let mut rows = s
        .store
        .aql(
            "FOR e IN @@has_version FILTER e._from == @n SORT e.seq DESC, e.created_at DESC
               RETURN {version: PARSE_IDENTIFIER(e._to).key, id: e._to,
                       collection: PARSE_IDENTIFIER(e._to).collection,
                       seq: e.seq, state: e.state, work_item: e.work_item,
                       created_at: e.created_at, created_by: e.created_by}",
            json!({"n": node}),
        )
        .await?;
    let cur = s.store.tree(None).await?.version(&node).map(str::to_string);
    let mut holding: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
    for pv in s.store.list_project_versions().await? {
        let id = pv["_id"].as_str().unwrap_or_default().to_string();
        if let Some(v) = s.store.tree(Some(&id)).await?.version(&node) {
            holding
                .entry(v.to_string())
                .or_default()
                .push(json!({"id": id, "name": pv["name"], "state": pv["state"]}));
        }
    }
    for r in rows.iter_mut() {
        let id = r["id"].as_str().unwrap_or_default().to_string();
        r["current"] = json!(cur.as_deref() == Some(id.as_str()));
        r["project_versions"] = json!(holding.get(&id).cloned().unwrap_or_default());
    }
    Ok(Json(json!(rows)))
}

// ---------------------------------------------------------------------- use cases

pub async fn list_use_cases(State(s): State<AppState>) -> ApiResult<Value> {
    Ok(Json(json!(s.store.list_use_cases().await?)))
}

pub async fn get_use_case(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    Ok(Json(s.store.get_use_case(&k).await?))
}

pub async fn create_use_case(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(b): Json<Value>,
) -> ApiResult<Value> {
    let uc = s.store.create_use_case(b, &p.id()).await?;
    s.events
        .emit("use_case.changed", json!({"key": uc["_key"]}));
    Ok(Json(uc))
}

pub async fn patch_use_case(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult<Value> {
    Ok(Json(s.store.update_use_case(&k, &b).await?))
}

#[derive(Deserialize)]
pub struct UcLink {
    /// relates_to (→ requirement) or expected_by (→ test symbol)
    pub edge: String,
    pub to: String,
}

pub async fn link_use_case(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(b): Json<UcLink>,
) -> ApiResult<Value> {
    s.store
        .link_use_case(
            &s.store.db.pid("use_cases", &k)?,
            &b.edge,
            &s.store.db.qid(&b.to)?,
        )
        .await?;
    Ok(Json(s.store.get_use_case(&k).await?))
}

#[derive(Deserialize)]
pub struct UcState {
    pub state: String,
}

/// Draft → live is a human decision, or an external assistant's last instruction (spec §15.2).
pub async fn set_use_case_state(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    Path(k): Path<String>,
    Json(b): Json<UcState>,
) -> ApiResult<Value> {
    if p.via == kodehub_store::identity::AuthVia::Internal && !crate::from_ui(&s, &headers) {
        return Err(ApiError {
            status: axum::http::StatusCode::FORBIDDEN,
            kind: "forbidden",
            message: "containerized agents cannot change a use case's state".into(),
        });
    }
    let uc = s.store.set_use_case_state(&k, &b.state, &p.id()).await?;
    s.events
        .emit("use_case.changed", json!({"key": k, "state": b.state}));
    Ok(Json(uc))
}

// ---------------------------------------------------------------------- test container types

pub async fn test_container_types(State(s): State<AppState>) -> ApiResult<Value> {
    Ok(Json(json!(s.store.test_container_types().await?)))
}

pub async fn put_test_container_type(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(name): Path<String>,
    Json(b): Json<Value>,
) -> ApiResult<Value> {
    Ok(Json(
        s.store.put_test_container_type(&name, b, &p.id()).await?,
    ))
}

pub async fn node_tests(State(s): State<AppState>, Query(q): Query<NodeQ>) -> ApiResult<Value> {
    Ok(Json(json!(s.store.tests_for_node(&q.node).await?)))
}
