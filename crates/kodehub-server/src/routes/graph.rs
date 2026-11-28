//! Generic graph access, code symbols, map, timeline and traceability queries.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Path, Query, State};
use axum::Json;
use kodehub_core::sig::parse_signature;
use kodehub_store::key_of;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub async fn health(State(s): State<AppState>) -> Json<Value> {
    let mut v = s.store.health().await;
    v["service"] = json!("kodehub-server");
    v["version"] = json!(env!("CARGO_PKG_VERSION"));
    Json(v)
}

#[derive(Deserialize)]
pub struct IdQuery {
    pub id: String,
}

pub async fn get_node(State(s): State<AppState>, Query(q): Query<IdQuery>) -> ApiResult<Value> {
    Ok(Json(s.store.get_node(&q.id).await?))
}

pub async fn get_node_path(
    State(s): State<AppState>,
    Path((c, k)): Path<(String, String)>,
) -> ApiResult<Value> {
    Ok(Json(s.store.get_node(&format!("{c}/{k}")).await?))
}

pub async fn list_nodes(
    State(s): State<AppState>,
    Path(c): Path<String>,
    Query(q): Query<BTreeMap<String, String>>,
) -> ApiResult<Vec<Value>> {
    let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(500);
    let filter: Map<String, Value> = q
        .into_iter()
        .filter(|(k, _)| !matches!(k.as_str(), "limit" | "project" | "token"))
        .map(|(k, v)| (k, json!(v)))
        .collect();
    Ok(Json(s.store.query_nodes(&c, &filter, limit).await?))
}

#[derive(Deserialize)]
pub struct QueryBody {
    pub collection: String,
    #[serde(default)]
    pub filter: Map<String, Value>,
    #[serde(default)]
    pub limit: Option<usize>,
}

pub async fn query_nodes(
    State(s): State<AppState>,
    Json(b): Json<QueryBody>,
) -> ApiResult<Vec<Value>> {
    Ok(Json(
        s.store
            .query_nodes(&b.collection, &b.filter, b.limit.unwrap_or(500))
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct TraverseBody {
    pub start: String,
    #[serde(default = "default_dir")]
    pub direction: String,
    #[serde(default)]
    pub edges: Vec<String>,
    #[serde(default)]
    pub depth: Option<u32>,
}
fn default_dir() -> String {
    "outbound".into()
}

pub async fn traverse(State(s): State<AppState>, Json(b): Json<TraverseBody>) -> ApiResult<Value> {
    Ok(Json(
        s.store
            .traverse(&b.start, &b.direction, &b.edges, b.depth.unwrap_or(3))
            .await?,
    ))
}

pub async fn create_node(
    State(s): State<AppState>,
    Path(c): Path<String>,
    Json(doc): Json<Value>,
) -> ApiResult<Value> {
    let v = s.store.insert_node(&c, doc).await?;
    s.events
        .emit("node.changed", json!({"id": v["_id"], "change": "created"}));
    Ok(Json(v))
}

pub async fn patch_node(
    State(s): State<AppState>,
    Path((c, k)): Path<(String, String)>,
    Json(p): Json<Value>,
) -> ApiResult<Value> {
    let v = s.store.update_node(&c, &k, &p).await?;
    s.events.emit(
        "node.changed",
        json!({"id": format!("{c}/{k}"), "change": "updated"}),
    );
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct EdgeBody {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub data: Value,
}

pub async fn create_edge(
    State(s): State<AppState>,
    Path(c): Path<String>,
    Json(b): Json<EdgeBody>,
) -> ApiResult<Value> {
    guard_api_edge(&c)?;
    let v = s.store.insert_edge(&c, &b.from, &b.to, b.data).await?;
    s.events.emit(
        "node.changed",
        json!({"id": b.from, "change": "edge", "edge": c}),
    );
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct EdgeQuery {
    pub from: String,
    pub to: String,
}

pub async fn delete_edge(
    State(s): State<AppState>,
    Path(c): Path<String>,
    Query(q): Query<EdgeQuery>,
) -> ApiResult<Value> {
    guard_api_edge(&c)?;
    s.store.remove_edge(&c, &q.from, &q.to).await?;
    s.events.emit(
        "node.changed",
        json!({"id": q.from, "change": "edge_removed", "edge": c}),
    );
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub struct SeedBody {
    pub path: String,
}

pub async fn seed(State(s): State<AppState>, Json(b): Json<SeedBody>) -> ApiResult<Value> {
    let r = kodehub_store::seed::import_dir(&s.store, std::path::Path::new(&b.path)).await?;
    s.events.emit("node.changed", json!({"change": "seeded"}));
    Ok(Json(serde_json::to_value(r).unwrap()))
}

pub async fn invariants(State(s): State<AppState>) -> ApiResult<Value> {
    let p = s.store.check_invariants().await?;
    Ok(Json(json!({"ok": p.is_empty(), "problems": p})))
}

/// Code and test nodes of the current project version, with their version there.
pub async fn list_symbols(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    let tree = s.store.tree(None).await?;
    let mut pairs = Vec::new();
    for coll in ["code", "tests"] {
        for n in tree.nodes_in(coll) {
            pairs.push(json!({"n": n, "v": tree.version(n)}));
        }
    }
    Ok(Json(
        s.store
            .aql(
                "FOR p IN @pairs LET s = DOCUMENT(p.n) LET v = DOCUMENT(p.v)
                 SORT s.language, s.display_name
                 RETURN MERGE(s, {version: v._key, signature: v.signature,
                                  lifecycle: s.lifecycle || 'live'})",
                json!({"pairs": pairs}),
            )
            .await?,
    ))
}

/// A code or test version by key.
pub async fn get_version(State(s): State<AppState>, Path(h): Path<String>) -> ApiResult<Value> {
    let k = s.store.db.pkey(key_of(&h))?;
    for c in ["code_versions", "test_versions"] {
        match s.store.db.get(c, &k, None).await {
            Ok(v) => return Ok(Json(v)),
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::not_found(format!("version {h}")))
}

/// Everything the node panel needs for one code or test node.
pub async fn symbol_detail(State(s): State<AppState>, Path(key): Path<String>) -> ApiResult<Value> {
    let id = s.store.symbol_id(&key).await?;
    let key = key_of(&id).to_string();
    let (coll, _) = id.split_once('/').unwrap_or_default();
    let sym = s.store.db.get(coll, &key, None).await?;
    let g = s.store.load_code_graph().await?;
    let cs = g.symbol(&key)?.clone();
    let vcoll = if coll == "tests" {
        "test_versions"
    } else {
        "code_versions"
    };
    let version = s.store.db.get(vcoll, &cs.version, None).await?;
    let history = s.store.version_history(&key).await?;
    let parsed = parse_signature(cs.language, version["signature"].as_str().unwrap_or(""));
    let (runnable, reason) = match kodehub_render::runnable(&g, &key) {
        Ok(_) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    let params: Vec<Value> = parsed
        .params
        .iter()
        .map(|p| {
            json!({"name": p.name, "raw": p.raw, "default": p.default,
                        "spec": kodehub_render::form_spec(&g, cs.language, &p.ty, 0)})
        })
        .collect();
    let impact = s.store.impact(&key).await?;
    let node_ref = |k: &str| kodehub_store::queries::node_id(&g, k);
    let deps: Vec<Value> = g
        .out_edges(&key)
        .filter(|e| e.kind != kodehub_core::model::CodeEdgeKind::TestedWith)
        .filter_map(|e| g.symbols.get(&e.to).map(|t| json!({"id": node_ref(&t.id), "key": t.id, "display_name": t.display_name, "kind": t.kind, "edge": e.kind.collection()})))
        .collect();
    let dependents: Vec<Value> = g
        .in_edges(&key)
        .filter(|e| e.kind != kodehub_core::model::CodeEdgeKind::TestedWith)
        .filter_map(|e| g.symbols.get(&e.from).map(|t| json!({"id": node_ref(&t.id), "key": t.id, "display_name": t.display_name, "kind": t.kind, "edge": e.kind.collection()})))
        .collect();
    let tests: std::collections::BTreeSet<String> = g.tests_reaching(&key).into_iter().collect();
    let states = s.store.test_states(&g).await?;
    let mut test_rows: Vec<Value> = tests
        .iter()
        .filter_map(|t| {
            let ts = g.symbols.get(t)?;
            let st = states.get(t);
            Some(
                json!({"id": t, "node": format!("tests/{t}"), "name": ts.display_name,
                        "language": ts.language,
                        "status": st.map(|x| x.status.clone()).unwrap_or_else(|| "not_run".into()),
                        "ran_at": st.and_then(|x| x.ran_at.clone())}),
            )
        })
        .collect();
    test_rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    // decisions: requirements of kind decision applying to this node or what contains it
    let tree = s.store.tree(None).await?;
    let anc = tree.ancestors(&id);
    let decisions = s
        .store
        .aql(
            "FOR e IN @@applies_to FILTER e._to IN @anc LET d = DOCUMENT(e._from)
               FILTER d.kind == 'decision' RETURN DISTINCT d",
            json!({"anc": anc}),
        )
        .await?;
    let runs = s
        .store
        .aql(
            "FOR r IN @@test_logs FILTER r.kind == 'run' AND (r.symbol == @k OR r.node == @id)
               SORT r.ran_at DESC LIMIT 8 RETURN r",
            json!({"k": key, "id": id}),
        )
        .await?;
    let component = match tree.parent.get(&id) {
        Some(p) => Some(s.store.get_node(p).await?),
        None => None,
    };
    let health = s.store.node_health().await?;
    Ok(Json(json!({
        "symbol": sym,
        "node": id,
        "version": version,
        "closure_hash": g.closure_hash(&key)?,
        "history": history,
        "signature": parsed,
        "form": {"runnable": runnable, "reason": reason, "params": params, "returns": parsed.returns},
        "requirements": impact["requirements"],
        "tags": impact["tags"],
        "dependencies": deps,
        "dependents": dependents,
        "tests": test_rows,
        "decisions": decisions,
        "runs": runs,
        "component": component,
        "health": health.get(&id).map(|h| h["health"].clone()).unwrap_or(json!("uncovered")),
    })))
}

#[derive(Deserialize)]
pub struct MapQuery {
    /// A project version (key or id): show the system as it is there. Default: the current one.
    pub at: Option<String>,
}

/// Map level of a node collection (code and tests draw as `symbol`).
fn map_level(coll: &str) -> &'static str {
    match coll {
        "contexts" => "context",
        "containers" => "container",
        "components" => "component",
        _ => "symbol",
    }
}

/// The system map of a project version: C4 nodes + code and tests with health, structural and
/// code edges, and (live view) ghost nodes from open proposals and C4 nodes awaiting placement.
pub async fn map(State(s): State<AppState>, Query(q): Query<MapQuery>) -> ApiResult<Value> {
    let store = &s.store;
    let current = store.current_version().await?;
    let pv = match &q.at {
        Some(a) => store.pv_id(a)?,
        None => current.clone(),
    };
    let live = pv == current;
    let tree = store.tree(Some(&pv)).await?;
    let cur_tree = if live {
        tree.clone()
    } else {
        store.tree(None).await?
    };
    let health = if live {
        store.node_health().await?
    } else {
        Default::default()
    };
    let project_node = store.project_node()?;
    let docs = store
        .aql(
            "FOR n IN UNION(
                (FOR x IN @@contexts RETURN x), (FOR x IN @@containers RETURN x),
                (FOR x IN @@components RETURN x), (FOR x IN @@code RETURN x),
                (FOR x IN @@tests RETURN x))
               LET cand = LENGTH(FOR e IN @@has_version FILTER e._from == n._id AND e.state == 'candidate' RETURN 1)
               RETURN MERGE(n, {candidates: cand})",
            json!({}),
        )
        .await?;
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut nodes: Vec<Value> = Vec::new();
    for n in docs {
        let id = n["_id"].as_str().unwrap_or_default().to_string();
        let coll = id.split_once('/').map(|x| x.0).unwrap_or_default();
        let level = map_level(coll);
        let in_tree = tree.version(&id).is_some();
        // C4 nodes awaiting a human (planned, unplaced) show in the live view only
        let planned = !in_tree && level != "symbol" && live;
        if !in_tree && !planned {
            continue;
        }
        let parent = tree
            .parent
            .get(&id)
            .filter(|p| **p != project_node)
            .cloned()
            .or_else(|| {
                n["planned_parent"]
                    .as_str()
                    .filter(|p| !p.starts_with("projects/"))
                    .map(str::to_string)
            });
        let version = (level == "symbol")
            .then(|| tree.version(&id).map(|v| key_of(v).to_string()))
            .flatten();
        let mut r = json!({
            "id": id, "key": n["_key"], "level": level,
            "label": n["display_name"].as_str().or(n["title"].as_str()).or(n["name"].as_str()).unwrap_or(key_of(&id)),
            "parent": parent, "kind": n["kind"], "language": n["language"], "tags": n["tags"],
            "description": n["description"], "version": version,
            "candidates": n["candidates"], "lifecycle_raw": n["lifecycle"],
        });
        if planned {
            r["planned"] = json!(true);
        }
        let h = health.get(&id);
        // history mode shows structure and what changed, not today's health (review M6 #21)
        r["health"] = if !live {
            json!("history")
        } else {
            h.map(|h| h["health"].clone()).unwrap_or(json!("uncovered"))
        };
        r["tests"] = h.map(|h| h["tests"].clone()).unwrap_or(json!(0));
        if !live && level == "symbol" {
            r["version_at"] = r["version"].clone();
            r["changed_since"] = json!(tree.version(&id) != cur_tree.version(&id));
        }
        ids.insert(id);
        nodes.push(r);
    }
    let mut out_edges = Vec::new();
    for coll in [
        "calls",
        "depends_on",
        "member_of",
        "tested_with",
        "binds_to",
    ] {
        let es = store
            .aql(
                "FOR e IN @@c RETURN {id: e._id, source: e._from, target: e._to, kind: @c2}",
                json!({"@c": coll, "c2": coll}),
            )
            .await?;
        out_edges.extend(es.into_iter().filter(|e| {
            ids.contains(e["source"].as_str().unwrap_or(""))
                && ids.contains(e["target"].as_str().unwrap_or(""))
        }));
    }
    // ghost nodes from open proposals (live view only)
    let open = if live {
        store.list_proposals(true).await?
    } else {
        vec![]
    };
    let mut ghosts = Vec::new();
    let mut proposed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for wp in &open {
        let wi = wp["work_item"].as_str().unwrap_or_default().to_string();
        if wp["state"] == "escalated" {
            continue;
        }
        let comp_id = |c: &Value| {
            c.as_str()
                .and_then(|c| store.db.pid("components", key_of(c)).ok())
        };
        let mut seen_new: BTreeSet<String> = BTreeSet::new();
        for (pk, p) in wp["proposals"].as_object().cloned().unwrap_or_default() {
            if p["new"] == json!(true) {
                seen_new.insert(p["display_name"].as_str().unwrap_or("").to_string());
                ghosts.push(json!({
                    "id": format!("ghost:{wi}:{pk}"), "key": pk, "level": "symbol", "ghost": true,
                    "label": p["display_name"], "kind": p["kind"], "language": p["language"],
                    "parent": comp_id(&p["component"]),
                    "work_item": wi, "health": "ghost", "action": "add",
                }));
            } else if let Some(n) = p["node"].as_str() {
                proposed.entry(n.to_string()).or_default().push(wi.clone());
            }
        }
        for ch in wp["plan"]["changes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let name = ch["display_name"].as_str().unwrap_or("").to_string();
            let action = ch["action"].as_str().unwrap_or("modify");
            if action == "add" && !seen_new.contains(&name) {
                ghosts.push(json!({
                    "id": format!("ghost:{wi}:plan:{name}"), "key": name, "level": "symbol", "ghost": true,
                    "label": name, "kind": ch["kind"], "language": ch["language"],
                    "parent": comp_id(&ch["component"]),
                    "work_item": wi, "health": "ghost", "action": "add", "description": ch["description"],
                }));
            } else if let Some(sym) = ch["symbol"].as_str() {
                if let Ok(n) = store.symbol_id(sym).await {
                    proposed.entry(n).or_default().push(wi.clone());
                }
            }
        }
    }
    for n in nodes.iter_mut() {
        if let Some(ws) = n["id"].as_str().and_then(|id| proposed.get(id)) {
            let mut v = ws.clone();
            v.sort();
            v.dedup();
            n["proposed_in"] = json!(v);
        }
    }
    nodes.extend(ghosts);
    let mission = store.get_philosophy().await?.map(|p| {
        json!({"_key": "philosophy", "title": p["title"], "statement": p["text"],
               "revision": p["revision"], "kind": "philosophy"})
    });
    // in-node badges (spec §8.3): lifecycle and requirement counts
    let lifecycle = store.node_lifecycle().await?;
    let req_counts = store.requirement_counts().await?;
    let test_counts = store.test_counts().await?;
    let busy = match store.project_id() {
        Some(p) => store.in_progress_by_node(&p).await?,
        None => Default::default(),
    };
    for n in nodes.iter_mut() {
        if let Some(id) = n["id"].as_str().map(str::to_string) {
            if let Some(l) = lifecycle.get(&id) {
                n["lifecycle"] = l["state"].clone();
            }
            if let Some(c) = req_counts.get(&id) {
                n["requirements"] = c.clone();
            }
            if let Some(c) = test_counts.get(&id) {
                n["test_counts"] = c.clone();
            }
            n["in_progress"] = json!(busy.get(&id).copied().unwrap_or(0));
        }
    }
    let pv_doc = store.get_project_version(&pv).await?;
    Ok(Json(json!({
        "mission": mission, "nodes": nodes, "edges": out_edges,
        "at": q.at, "project_version": {"id": pv, "name": pv_doc["name"], "state": pv_doc["state"],
                                         "current": live},
    })))
}

/// Project versions (opened, frozen) and accepted proposals, oldest first — the time slider's
/// stops. Each stop names the project version to show.
pub async fn timeline(State(s): State<AppState>) -> ApiResult<Value> {
    let rows = s
        .store
        .aql(
            "LET pvs = (FOR v IN @@project_versions
                          RETURN {kind: v.state == 'frozen' ? 'frozen' : 'version', id: v._id, key: v._key,
                                  label: v.name, at: v.frozen_at || v.created_at, project_version: v._id,
                                  symbols: []})
             LET accepts = (FOR w IN @@w FILTER w.project == @p AND w.proposal.state == 'accepted'
                              RETURN {kind: 'accept', id: w._id, key: w._key,
                                      label: w.proposal.title || w.title || w.kind, at: w.proposal.accepted_at,
                                      project_version: w.proposal.accepted_into,
                                      symbols: ATTRIBUTES(w.proposal.accepted_versions || {})})
             FOR e IN UNION(pvs, accepts) SORT e.at RETURN e",
            json!({"@w": "work_items", "p": s.store.db.require_project()?}),
        )
        .await?;
    Ok(Json(json!({"events": rows})))
}

#[derive(Deserialize)]
pub struct CoverageQuery {
    pub requirement: Option<String>,
}

pub async fn coverage(
    State(s): State<AppState>,
    Query(q): Query<CoverageQuery>,
) -> ApiResult<Value> {
    let c = s
        .store
        .coverage(q.requirement.as_deref().map(key_of))
        .await?;
    Ok(Json(serde_json::to_value(c).unwrap()))
}

pub async fn orphans(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    Ok(Json(s.store.orphans().await?))
}

pub async fn impact(State(s): State<AppState>, Path(sym): Path<String>) -> ApiResult<Value> {
    let key = resolve_symbol(&s, &sym).await?;
    Ok(Json(s.store.impact(&key).await?))
}

#[derive(Deserialize)]
pub struct DupQuery {
    /// Also return near duplicates with at least this similarity (0–1).
    pub near: Option<f64>,
}

pub async fn duplicates(State(s): State<AppState>, Query(q): Query<DupQuery>) -> ApiResult<Value> {
    let exact = s.store.duplicates().await?;
    match q.near {
        None => Ok(Json(json!(exact))),
        Some(t) => Ok(Json(
            json!({"exact": exact, "near": s.store.near_duplicates(t.clamp(0.1, 1.0)).await?}),
        )),
    }
}

pub async fn node_health(State(s): State<AppState>) -> ApiResult<Value> {
    Ok(Json(
        serde_json::to_value(s.store.node_health().await?).unwrap(),
    ))
}

/// Accept a code/test key, id or a display name; returns the (qualified) key.
pub async fn resolve_symbol(s: &AppState, name: &str) -> Result<String, ApiError> {
    if let Ok(id) = s.store.symbol_id(name).await {
        return Ok(key_of(&id).to_string());
    }
    let k = key_of(name);
    let doc = s.store.symbol_by_name(None, k).await?;
    Ok(doc["_key"].as_str().unwrap_or_default().to_string())
}

/// Code and trace edges change what is built and what counts as tested, so through the API they
/// only change in a work item's proposal (`propose_edge`), never directly (final review #9).
/// Structure and versions belong to the version tree and are never written as raw edges.
fn guard_api_edge(collection: &str) -> Result<(), ApiError> {
    const PROPOSAL_ONLY: &[&str] = &[
        "calls",
        "depends_on",
        "member_of",
        "tested_with",
        "binds_to",
        "verified_by",
    ];
    if PROPOSAL_ONLY.contains(&collection) {
        return Err(ApiError::bad_request(format!(
            "`{collection}` edges change code or coverage — propose them in a work item's proposal (propose_edge)"
        )));
    }
    if matches!(collection, "has_version" | "based_on") {
        return Err(ApiError::bad_request(format!(
            "`{collection}` edges belong to the version tree and are written by the orchestrator"
        )));
    }
    Ok(())
}

/// Common contexts offered when creating a project.
pub async fn common_contexts() -> ApiResult<Value> {
    Ok(Json(json!(crate::routes::project_init::COMMON_CONTEXTS
        .iter()
        .map(|(n, d)| json!({"name": n, "description": d}))
        .collect::<Vec<_>>())))
}
