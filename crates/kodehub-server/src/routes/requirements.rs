//! Requirements (the main human input), missions and decisions.

use axum::extract::{Path, State};
use axum::Json;
use kodehub_store::key_of;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

/// The philosophy document, in the v0.05 mission shape (`statement`) for older clients.
pub async fn missions(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    Ok(Json(
        s.store
            .get_philosophy()
            .await?
            .map(|p| {
                let mut v = p.clone();
                v["statement"] = p["text"].clone();
                v["scope"] = json!("program");
                vec![v]
            })
            .unwrap_or_default(),
    ))
}

/// Requirements with anatomy and coverage, plus the group tree.
pub async fn list(State(s): State<AppState>) -> ApiResult<Value> {
    let cov = s.store.coverage(None).await?;
    let docs = s
        .store
        .aql("FOR r IN @@requirements SORT r.title RETURN r", json!({}))
        .await?;
    let reqs: Vec<Value> = docs
        .into_iter()
        .map(|mut d| {
            if let Some(c) = cov
                .requirements
                .iter()
                .find(|c| c.id == d["_id"].as_str().unwrap_or(""))
            {
                d["status"] = json!(c.status);
                d["tests"] = serde_json::to_value(&c.tests).unwrap();
                d["satisfiers"] = json!(c.satisfiers);
            }
            d
        })
        .collect();
    let groups: Vec<Value> = cov
        .groups
        .iter()
        .map(|g| serde_json::to_value(g).unwrap())
        .collect();
    let mut summary = json!(cov.summary);
    summary["total"] = json!(reqs.len());
    Ok(Json(
        json!({"requirements": reqs, "groups": groups, "summary": summary}),
    ))
}

pub async fn get(State(s): State<AppState>, Path(key): Path<String>) -> ApiResult<Value> {
    let key = key_of(&key).to_string();
    let mut doc = s.store.db.get("requirements", &key, None).await?;
    let cov = s.store.coverage(Some(&key)).await?;
    if let Some(c) = cov.requirements.first() {
        doc["status"] = json!(c.status);
        doc["tests"] = serde_json::to_value(&c.tests).unwrap();
        doc["satisfiers"] = json!(c.satisfiers);
    }
    // the full path for map highlighting: satisfiers, everything beneath them, tests, decisions
    let path = s
        .store
        .aql(
            "LET r = CONCAT(@pfx, 'requirements/', @k)
             LET sat = (FOR e IN @@satisfies FILTER e._to == r RETURN e._from)
             LET groups = (FOR e IN @@includes FILTER e._to == r RETURN e._from)
             LET gsat = (FOR g IN groups FOR e IN @@satisfies FILTER e._to == g RETURN e._from)
             LET beneath = UNIQUE(FOR x IN sat FOR v IN 0..30 OUTBOUND x @@contains, @@calls, @@depends_on, @@member_of
                                    OPTIONS {uniqueVertices: 'global', order: 'bfs'} RETURN v._id)
             LET tests = UNIQUE(APPEND((FOR e IN @@verifies FILTER e._to == r RETURN e._from),
                                       (FOR id IN beneath FOR e IN @@tested_by FILTER e._from == id RETURN e._to)))
             LET decisions = UNIQUE(FOR id IN APPEND(beneath, sat) FOR e IN @@justified_by FILTER e._from == id RETURN e._to)
             RETURN {satisfiers: sat, beneath, tests, decisions, groups, group_satisfiers: gsat}",
            json!({"k": key}),
        )
        .await?;
    doc["path"] = path.into_iter().next().unwrap_or(json!({}));
    let history = s.store.requirement_history(&key).await?;
    doc["history_count"] = json!(history.len());
    Ok(Json(doc))
}

pub async fn history(State(s): State<AppState>, Path(key): Path<String>) -> ApiResult<Vec<Value>> {
    Ok(Json(s.store.requirement_history(key_of(&key)).await?))
}

pub async fn create(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(mut body): Json<Value>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let group = body.as_object_mut().and_then(|o| o.remove("group"));
    let satisfied_by = body.as_object_mut().and_then(|o| o.remove("satisfied_by"));
    let doc = s.store.create_requirement(body).await?;
    let id = doc["_id"].as_str().unwrap_or_default().to_string();
    if let Some(g) = group.as_ref().and_then(Value::as_str) {
        s.store
            .insert_edge(
                "includes",
                &format!("requirement_groups/{}", key_of(g)),
                &id,
                json!({}),
            )
            .await?;
    }
    for sat in satisfied_by
        .as_ref()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if let Some(n) = sat.as_str() {
            s.store.insert_edge("satisfies", n, &id, json!({})).await?;
        }
    }
    s.events
        .emit("node.changed", json!({"id": id, "change": "created"}));
    Ok(Json(doc))
}

#[derive(Deserialize, Default)]
pub struct EditMeta {
    #[serde(default)]
    pub by: Option<String>,
    /// Skip auto-opening a changeset (e.g. typo fixes).
    #[serde(default)]
    pub no_changeset: bool,
}

async fn apply_edit(
    s: &AppState,
    key: &str,
    patch: Value,
    meta: EditMeta,
) -> Result<Value, ApiError> {
    let by = meta.by.unwrap_or_else(|| "human".into());
    let (doc, changed) = s.store.update_requirement(key, &patch, &by, true).await?;
    if changed {
        s.events.emit(
            "node.changed",
            json!({"id": doc["_id"], "change": "requirement_edited", "revision": doc["revision"]}),
        );
        if !meta.no_changeset {
            s.requirement_edited(key, &by);
        }
    }
    Ok(json!({"requirement": doc, "changed": changed}))
}

pub async fn update(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
    Json(mut body): Json<Value>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let meta: EditMeta = body
        .as_object_mut()
        .and_then(|o| o.remove("_meta"))
        .and_then(|m| serde_json::from_value(m).ok())
        .unwrap_or_default();
    let patch = body.get("patch").cloned().unwrap_or(body);
    Ok(Json(apply_edit(&s, key_of(&key), patch, meta).await?))
}

#[derive(Deserialize)]
pub struct ExampleBody {
    pub description: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub expected: Value,
    #[serde(default)]
    pub by: Option<String>,
    /// Run that produced this example (Save as example).
    #[serde(default)]
    pub run: Option<String>,
}

async fn append(s: &AppState, key: &str, field: &str, b: ExampleBody) -> Result<Value, ApiError> {
    if b.description.trim().is_empty() {
        return Err(ApiError::bad_request("description is required"));
    }
    let cur = s.store.db.get("requirements", key, None).await?;
    let mut list = cur[field].as_array().cloned().unwrap_or_default();
    let mut entry = json!({"description": b.description, "input": b.input, "expected": b.expected});
    if let Some(r) = b.run {
        entry["from_run"] = json!(r);
    }
    list.push(entry);
    apply_edit(
        s,
        key,
        json!({field: list}),
        EditMeta {
            by: b.by,
            no_changeset: false,
        },
    )
    .await
}

pub async fn add_edge_case(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
    Json(b): Json<ExampleBody>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    Ok(Json(append(&s, key_of(&key), "edge_cases", b).await?))
}

pub async fn add_example(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(key): Path<String>,
    Json(b): Json<ExampleBody>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    Ok(Json(append(&s, key_of(&key), "examples", b).await?))
}

pub async fn list_decisions(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    Ok(Json(
        s.store
            .aql(
                "FOR d IN @@decisions SORT d.created_at DESC
                 RETURN MERGE(d, {justifies: (FOR e IN @@justified_by FILTER e._to == d._id RETURN e._from)})",
                json!({}),
            )
            .await?,
    ))
}

pub async fn create_decision(
    State(s): State<AppState>,
    Json(mut body): Json<Value>,
) -> ApiResult<Value> {
    let justifies: Vec<String> = body
        .as_object_mut()
        .and_then(|o| o.remove("justifies"))
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let d = s.store.create_decision(body, &justifies).await?;
    s.events.emit(
        "node.changed",
        json!({"id": d["_id"], "change": "decision"}),
    );
    Ok(Json(d))
}
