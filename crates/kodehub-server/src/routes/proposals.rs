//! Work-item proposal routes (D-058; the v0.1 changeset lifecycle). A code change is the
//! proposal of a work item: `/api/work-items/{key}/proposal/…`. Agents drive these through the
//! same API; humans approve plans, review, resolve escalations and request changes.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use kodehub_core::model::ProposalState;
use kodehub_store::proposals::ProposeVersion;
use kodehub_store::queue::NewWorkItem;
use kodehub_store::Principal;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::ProjectCtx;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub open: Option<bool>,
}

/// Proposals of the project's work items (newest first).
pub async fn list(State(s): State<AppState>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Value>> {
    Ok(Json(s.store.list_proposals(q.open.unwrap_or(false)).await?))
}

#[derive(Deserialize)]
pub struct OpenBody {
    pub title: String,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub requirements: Vec<String>,
    #[serde(default)]
    pub by: Option<String>,
    /// The open project version the change goes into (default: the current one).
    #[serde(default)]
    pub project_version: Option<String>,
    /// The node the change is about.
    #[serde(default)]
    pub target: Option<String>,
}

fn updated(s: &AppState, v: &Value) {
    s.events.emit(
        "proposal.updated",
        json!({"key": v["_key"], "state": v["state"]}),
    );
}

/// Request a code change by hand: a coding work item with an open proposal.
pub async fn open(
    State(s): State<AppState>,
    Extension(p): Extension<ProjectCtx>,
    Extension(who): Extension<Principal>,
    Json(b): Json<OpenBody>,
) -> ApiResult<Value> {
    if b.title.trim().is_empty() {
        return Err(ApiError::bad_request("title is required"));
    }
    if let Some(v) = &b.project_version {
        let d = s.store.get_node(&s.store.pv_id(v)?).await?;
        if d["state"] != "open" {
            return Err(ApiError::bad_request(format!(
                "{} is frozen; changes go into an open project version (or a hotfix of it)",
                d["name"].as_str().unwrap_or(v)
            )));
        }
    }
    let w = s
        .store
        .enqueue(&NewWorkItem {
            project: p.id.clone(),
            agent_type: "coding".into(),
            kind: "change".into(),
            target: b.target.as_deref().map(|t| s.store.db.qid(t)).transpose()?,
            task: json!({"title": b.title, "intent": b.intent}),
            requested_by: Some(who.id()),
            project_version: b.project_version.clone(),
            ..Default::default()
        })
        .await?;
    let k = w["_key"].as_str().unwrap_or_default().to_string();
    let v = s
        .store
        .open_proposal(
            &k,
            &b.title,
            &b.intent,
            &b.requirements,
            b.by.as_deref().unwrap_or("human"),
        )
        .await?;
    updated(&s, &v);
    Ok(Json(v))
}

/// Attach a proposal to an existing work item.
pub async fn open_on(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(b): Json<OpenBody>,
) -> ApiResult<Value> {
    let v = s
        .store
        .open_proposal(
            &k,
            &b.title,
            &b.intent,
            &b.requirements,
            b.by.as_deref().unwrap_or("orchestrator"),
        )
        .await?;
    updated(&s, &v);
    Ok(Json(v))
}

pub async fn get(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    Ok(Json(s.store.get_proposal(&k).await?))
}

#[derive(Deserialize)]
pub struct PlanBody {
    pub plan: Value,
    #[serde(default)]
    pub by: Option<String>,
}

pub async fn plan(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(b): Json<PlanBody>,
) -> ApiResult<Value> {
    let v = s
        .store
        .set_plan(&k, b.plan, b.by.as_deref().unwrap_or("orchestrator"))
        .await?;
    updated(&s, &v);
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
pub struct ByBody {
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn approve(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(k): Path<String>,
    body: Option<Json<ByBody>>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let b = body.map(|b| b.0).unwrap_or_default();
    let v = s
        .store
        .transition(
            &k,
            ProposalState::Approved,
            b.by.as_deref().unwrap_or("human"),
            b.note.as_deref().unwrap_or("plan approved"),
        )
        .await?;
    updated(&s, &v);
    s.execute_proposal(&k);
    Ok(Json(v))
}

pub async fn propose_version(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(b): Json<ProposeVersion>,
) -> ApiResult<Value> {
    let v = s.store.propose_version(&k, &b).await?;
    s.events.emit(
        "proposal.updated",
        json!({"key": k, "proposal": v["symbol"]}),
    );
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct EdgeBody {
    pub collection: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub data: Value,
}

pub async fn propose_edge(
    State(s): State<AppState>,
    Path(k): Path<String>,
    Json(b): Json<EdgeBody>,
) -> ApiResult<Value> {
    let v = s
        .store
        .propose_edge(&k, &b.collection, &b.from, &b.to, b.data)
        .await?;
    s.events.emit("proposal.updated", json!({"key": k}));
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct ReviewBody {
    pub findings: Vec<Value>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub by: Option<String>,
}

pub async fn submit_review(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(k): Path<String>,
    Json(b): Json<ReviewBody>,
) -> ApiResult<Value> {
    // reviews over the API are advisory unless a human submits them from the UI (review M6 #4)
    let source = if crate::from_ui(&s, &headers) {
        "human"
    } else {
        "api"
    };
    let v = s
        .store
        .submit_review(
            &k,
            &b.findings,
            &b.summary,
            b.by.as_deref().unwrap_or("reviewer"),
            source,
        )
        .await?;
    s.events
        .emit("proposal.updated", json!({"key": k, "review": v["round"]}));
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct TransitionBody {
    pub to: ProposalState,
    #[serde(default)]
    pub by: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn transition(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(k): Path<String>,
    Json(b): Json<TransitionBody>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    // the engine owns testing/ready/accepted; `accepted` only ever happens inside acceptance
    // (review M6 #2)
    if !matches!(
        b.to,
        ProposalState::Approved
            | ProposalState::Building
            | ProposalState::Reviewing
            | ProposalState::Rejected
            | ProposalState::Escalated
    ) {
        return Err(ApiError::bad_request(format!(
            "`{}` can't be set directly; use accept / test / the agent workflow",
            b.to.as_str()
        )));
    }
    let v = s
        .store
        .transition(
            &k,
            b.to,
            b.by.as_deref().unwrap_or("human"),
            b.note.as_deref().unwrap_or(""),
        )
        .await?;
    updated(&s, &v);
    Ok(Json(v))
}

/// Run every affected test against the proposal's overlay closure.
pub async fn test(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    Ok(Json(run_proposal_tests(&s, &k).await?))
}

pub async fn run_proposal_tests(s: &AppState, k: &str) -> Result<Value, ApiError> {
    // human-driven path: reviewing → testing → (ready when tests pass and the review counts)
    let st = s.store.get_proposal(k).await?["state"].clone();
    if st == json!("reviewing") {
        s.store
            .transition(k, ProposalState::Testing, "human", "tests requested")
            .await?;
    }
    let (g, touched) = s.store.overlay_graph(k).await?;
    let affected = s.store.affected_tests(k, &g, &touched).await?;
    let outcomes = s
        .runner
        .run_tests(&s.store, &g, &affected, false, Some(k), None)
        .await?;
    for o in &outcomes {
        s.events.emit("test.result", json!({"test": o.test, "name": o.name, "status": o.status, "work_item": k, "cached": o.cached}));
    }
    let results = serde_json::to_value(&outcomes).unwrap();
    s.store
        .patch_proposal(k, json!({"test_results": results}))
        .await?;
    s.events.emit(
        "proposal.updated",
        json!({"key": k, "tests": outcomes.len()}),
    );
    let passed = outcomes.iter().all(|o| o.status == "pass") && !outcomes.is_empty();
    let mut state = s.store.get_proposal(k).await?["state"].clone();
    if passed && state == json!("testing") {
        let (checks, _, _) = s.store.accept_checks(k).await?;
        if checks.review_ok && checks.requirements_covered {
            let v = s
                .store
                .transition(
                    k,
                    ProposalState::Ready,
                    "human",
                    "tests pass, review attested",
                )
                .await?;
            state = v["state"].clone();
            updated(s, &v);
        }
    }
    Ok(json!({"affected": affected, "results": results, "passed": passed, "state": state}))
}

pub async fn checks(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    let c = s.store.accept_proposal(&k, "dry-run", true).await?;
    Ok(Json(json!({"ok": c.ok(), "checks": c})))
}

pub async fn accept(
    State(s): State<AppState>,
    Path(k): Path<String>,
    body: Option<Json<ByBody>>,
) -> ApiResult<Value> {
    let by = body.and_then(|b| b.0.by).unwrap_or_else(|| "human".into());
    let c = s.store.accept_proposal(&k, &by, false).await?;
    let p = s.store.get_proposal(&k).await?;
    updated(&s, &p);
    let _ = s.store.settle_proposal_item(&k, true, "accepted").await;
    for (sym, x) in p["proposals"].as_object().cloned().unwrap_or_default() {
        s.events.emit(
            "node.changed",
            json!({"id": x["node"].as_str().map(str::to_string).unwrap_or(format!("code/{sym}")),
                   "change": "accepted", "work_item": k}),
        );
    }
    Ok(Json(json!({"accepted": true, "checks": c, "proposal": p})))
}

pub async fn reject(
    State(s): State<AppState>,
    Path(k): Path<String>,
    body: Option<Json<ByBody>>,
) -> ApiResult<Value> {
    let b = body.map(|b| b.0).unwrap_or_default();
    let v = s
        .store
        .reject_proposal(
            &k,
            b.by.as_deref().unwrap_or("human"),
            b.reason.as_deref().unwrap_or("rejected"),
        )
        .await?;
    updated(&s, &v);
    let _ = s.store.settle_proposal_item(&k, false, "rejected").await;
    Ok(Json(v))
}

/// Resume the agent workflow for a proposal (after an error, or to re-plan a draft).
pub async fn run(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    if s.engine.is_none() {
        return Err(ApiError::bad_request("agents are not configured"));
    }
    let p = s.store.get_proposal(&k).await?;
    s.execute_proposal(&k);
    Ok(Json(json!({"started": true, "state": p["state"]})))
}

/// Ask the orchestrator to (re)plan a draft or planned proposal.
pub async fn replan(State(s): State<AppState>, Path(k): Path<String>) -> ApiResult<Value> {
    let e = s
        .engine
        .clone()
        .ok_or_else(|| ApiError::bad_request("agents are not configured"))?;
    let kk = k.clone();
    kodehub_store::spawn_scoped(async move {
        let _ = e.plan(&kk).await;
    });
    Ok(Json(json!({"started": true, "work_item": k})))
}

#[derive(Deserialize)]
pub struct ManualEditBody {
    pub symbol: String,
    pub body: String,
    #[serde(default)]
    pub signature: Option<String>,
}

/// Human text-view edit → a work item whose proposal goes straight to blind review and tests.
pub async fn manual_edit(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Extension(p): Extension<ProjectCtx>,
    Json(b): Json<ManualEditBody>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let key = crate::routes::graph::resolve_symbol(&s, &b.symbol).await?;
    let node = s.store.symbol_id(&key).await?;
    let detail = s.store.symbol_by_name(None, &key).await?;
    let name = detail["display_name"].as_str().unwrap_or(&key).to_string();
    let g = s.store.load_code_graph().await?;
    let cur = g.version(&g.symbol(&key)?.version)?.clone();
    let reqs: Vec<String> = s.store.impact(&key).await?["requirements"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            r["id"]
                .as_str()
                .map(|i| kodehub_store::key_of(i).to_string())
        })
        .collect();
    let w = s
        .store
        .enqueue(&NewWorkItem {
            project: p.id.clone(),
            agent_type: "coding".into(),
            kind: "manual_edit".into(),
            target: Some(node),
            task: json!({"note": format!("Hand edit of {name} from the text view")}),
            requested_by: Some("human".into()),
            ..Default::default()
        })
        .await?;
    let ck = w["_key"].as_str().unwrap_or_default().to_string();
    // the item is the human's own change: no agent claims it, the proposal drives it
    s.store
        .patch_work_item(&ck, json!({"state": "waiting_human"}))
        .await?;
    s.store
        .open_proposal(
            &ck,
            &format!("Manual edit: {name}"),
            &format!("Hand edit of {name} from the text view."),
            &reqs,
            "human",
        )
        .await?;
    s.store
        .set_plan(&ck, json!({"summary": format!("Manual edit of {name}"), "changes": [{"symbol": key, "display_name": name, "action": "modify", "description": "edited in the text view"}]}), "human")
        .await?;
    for (to, note) in [
        (ProposalState::Approved, "manual edit"),
        (ProposalState::Building, "manual edit"),
    ] {
        s.store.transition(&ck, to, "human", note).await?;
    }
    s.store
        .propose_version(
            &ck,
            &ProposeVersion {
                symbol: Some(key.clone()),
                signature: b.signature.unwrap_or(cur.signature),
                body: b.body,
                attributes: Some(cur.attributes),
                by: Some("human".into()),
                ..Default::default()
            },
        )
        .await?;
    let v = s
        .store
        .transition(
            &ck,
            ProposalState::Reviewing,
            "human",
            "submitted for blind review",
        )
        .await?;
    updated(&s, &v);
    s.execute_proposal(&ck);
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct ResolveBody {
    /// What the human decided (recorded as a decision).
    pub choice: String,
    #[serde(default)]
    pub rationale: String,
    /// `building` (send back to the coder) or `testing` (accept the code; findings overridden).
    pub to: String,
}

/// Resolve an escalated proposal: the human's call becomes a decision (review M6 #9).
pub async fn resolve(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(k): Path<String>,
    Json(b): Json<ResolveBody>,
) -> ApiResult<Value> {
    if !crate::from_ui(&s, &headers) {
        return Err(ApiError {
            status: axum::http::StatusCode::FORBIDDEN,
            kind: "human_only",
            message: "escalations are resolved by a human in the app".into(),
        });
    }
    let cs = s.store.get_proposal(&k).await?;
    if cs["state"] != json!("escalated") {
        return Err(ApiError::bad_request("the proposal is not escalated"));
    }
    let to = match b.to.as_str() {
        "building" => ProposalState::Building,
        "testing" => ProposalState::Testing,
        _ => return Err(ApiError::bad_request("to must be building or testing")),
    };
    let mut existing: Vec<String> = Vec::new();
    for x in cs["proposals"]
        .as_object()
        .map(|o| o.values().cloned().collect::<Vec<_>>())
        .unwrap_or_default()
    {
        if let Some(n) = x["node"].as_str() {
            if s.store.get_node(n).await.is_ok() {
                existing.push(n.to_string());
            }
        }
    }
    let d = s
        .store
        .create_decision(
            json!({"title": format!("Escalation resolved: {}", cs["title"].as_str().unwrap_or(&k)),
        "choice": b.choice, "rationale": b.rationale, "made_by": "human"}),
            &existing,
        )
        .await?;
    if to == ProposalState::Testing {
        // the human accepts the code: open findings are overridden by the decision
        let mut upd = std::collections::BTreeMap::new();
        for r in cs["reviews"].as_array().cloned().unwrap_or_default() {
            for f in r["findings"].as_array().cloned().unwrap_or_default() {
                if f["status"] == "open" {
                    upd.insert(
                        f["id"].as_str().unwrap_or_default().to_string(),
                        (
                            "resolved".to_string(),
                            format!(
                                "overridden by human decision {}",
                                d["_key"].as_str().unwrap_or("")
                            ),
                        ),
                    );
                }
            }
        }
        s.store.resolve_findings(&k, &upd).await?;
        // a human acceptance is an attested review of the current proposals
        let latest = s.store.get_proposal(&k).await?["reviews"]
            .as_array()
            .and_then(|r| r.last().cloned())
            .unwrap_or(json!({}));
        s.store
            .submit_review(
                &k,
                latest["findings"]
                    .as_array()
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
                "accepted by human on escalation",
                "human",
                "human",
            )
            .await?;
    } else {
        s.store.patch_proposal(&k, json!({"rework": {"findings": [], "failures": format!("Human decision: {}", b.choice)}})).await?;
    }
    let mut decisions = cs["decisions"].as_array().cloned().unwrap_or_default();
    decisions.push(d["_id"].clone());
    s.store
        .patch_proposal(&k, json!({"decisions": decisions}))
        .await?;
    let v = s
        .store
        .transition(&k, to, "human", "escalation resolved")
        .await?;
    updated(&s, &v);
    s.execute_proposal(&k);
    Ok(Json(v))
}
