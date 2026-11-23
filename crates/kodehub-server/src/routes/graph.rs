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
        .filter(|(k, _)| k != "limit")
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

pub async fn list_symbols(State(s): State<AppState>) -> ApiResult<Vec<Value>> {
    Ok(Json(
        s.store
            .aql(
                "FOR s IN @@symbols LET v = FIRST(FOR e IN @@current_version FILTER e._to == s._id RETURN DOCUMENT(e._from))
                 SORT s.language, s.display_name
                 RETURN MERGE(s, {version: v._key, signature: v.signature})",
                json!({}),
            )
            .await?,
    ))
}

pub async fn get_version(State(s): State<AppState>, Path(h): Path<String>) -> ApiResult<Value> {
    Ok(Json(s.store.db.get("versions", &h, None).await?))
}

/// Everything the node panel needs for one symbol.
pub async fn symbol_detail(State(s): State<AppState>, Path(key): Path<String>) -> ApiResult<Value> {
    let key = key_of(&key).to_string();
    let sym = s.store.db.get("symbols", &key, None).await?;
    let g = s.store.load_code_graph().await?;
    let cs = g.symbol(&key)?.clone();
    let version = s.store.db.get("versions", &cs.version, None).await?;
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
    let deps: Vec<Value> = g
        .out_edges(&key)
        .filter(|e| e.kind != kodehub_core::model::CodeEdgeKind::TestedBy)
        .filter_map(|e| g.symbols.get(&e.to).map(|t| json!({"id": t.id, "display_name": t.display_name, "kind": t.kind, "edge": e.kind.collection()})))
        .collect();
    let dependents: Vec<Value> = g
        .in_edges(&key)
        .filter(|e| e.kind != kodehub_core::model::CodeEdgeKind::TestedBy)
        .filter_map(|e| g.symbols.get(&e.from).map(|t| json!({"id": t.id, "display_name": t.display_name, "kind": t.kind, "edge": e.kind.collection()})))
        .collect();
    let tests: Vec<String> = g.tests_reaching(&key).into_iter().collect();
    let test_rows = s
        .store
        .aql(
            "FOR k IN @tests LET t = DOCUMENT(@@symbols, k)
               LET cv = FIRST(FOR e IN @@current_version FILTER e._to == t._id RETURN PARSE_IDENTIFIER(e._from).key)
               LET run = FIRST(FOR r IN @@test_runs FILTER r.test_version == cv AND r.closure_hash == t.closure_hash SORT r.ran_at DESC LIMIT 1 RETURN r)
               SORT t.display_name
               RETURN {id: t._key, name: t.display_name, language: t.language, status: run ? run.status : 'not_run', output: run.output, ran_at: run.ran_at}",
            json!({"tests": tests}),
        )
        .await?;
    let decisions = s
        .store
        .aql(
            "FOR e IN @@justified_by FILTER e._from == @s RETURN DOCUMENT(e._to)",
            json!({"s": format!("symbols/{key}")}),
        )
        .await?;
    let runs = s
        .store
        .aql(
            "FOR r IN @@runs FILTER r.symbol == @k SORT r.ran_at DESC LIMIT 8 RETURN r",
            json!({"k": key}),
        )
        .await?;
    let component = s
        .store
        .aql(
            "FOR e IN @@contains FILTER e._to == @s RETURN DOCUMENT(e._from)",
            json!({"s": format!("symbols/{key}")}),
        )
        .await?;
    let health = s.store.node_health().await?;
    Ok(Json(json!({
        "symbol": sym,
        "version": version,
        "closure_hash": g.closure_hash(&key)?,
        "history": history,
        "signature": parsed,
        "form": {"runnable": runnable, "reason": reason, "params": params, "returns": parsed.returns},
        "requirements": impact["requirements"],
        "groups": impact["groups"],
        "dependencies": deps,
        "dependents": dependents,
        "tests": test_rows,
        "decisions": decisions,
        "runs": runs,
        "component": component.first(),
        "health": health.get(&format!("symbols/{key}")).map(|h| h["health"].clone()).unwrap_or(json!("uncovered")),
    })))
}

#[derive(Deserialize)]
pub struct MapQuery {
    /// ISO timestamp: show the system as it was then.
    pub at: Option<String>,
}

/// The system map: C4 nodes + symbols with health, structural and code edges, ghost nodes.
pub async fn map(State(s): State<AppState>, Query(q): Query<MapQuery>) -> ApiResult<Value> {
    let store = &s.store;
    let health = store.node_health().await?;
    let rows = store
        .aql(
            "LET parents = (FOR e IN @@contains RETURN {child: e._to, parent: e._from})
             FOR n IN UNION(
                (FOR x IN @@contexts RETURN MERGE(x, {level: 'context'})),
                (FOR x IN @@containers RETURN MERGE(x, {level: 'container'})),
                (FOR x IN @@components RETURN MERGE(x, {level: 'component'})),
                (FOR x IN @@symbols RETURN MERGE(x, {level: 'symbol'})))
               FILTER @at == null OR n.created_at == null OR n.created_at <= @at
               LET parent = FIRST(FOR p IN parents FILTER p.child == n._id AND !STARTS_WITH(p.parent, CONCAT(@pfx, 'missions/')) RETURN p.parent)
               LET cv = n.level == 'symbol' ? FIRST(FOR e IN @@current_version FILTER e._to == n._id RETURN e) : null
               LET at_v = (@at == null || n.level != 'symbol') ? null : FIRST(
                    APPEND(
                      (FOR e IN @@previous_version FILTER e._to == n._id AND e.current_since <= @at AND e.superseded_at > @at RETURN e._from),
                      (FOR e IN @@current_version FILTER e._to == n._id AND e.since <= @at RETURN e._from)))
               RETURN {id: n._id, key: n._key, level: n.level, label: n.display_name || n.title || n._key,
                       parent, kind: n.kind, language: n.language, tags: n.tags, description: n.description,
                       version: cv ? PARSE_IDENTIFIER(cv._from).key : null,
                       version_at: at_v ? PARSE_IDENTIFIER(at_v).key : null}",
            json!({"at": q.at}),
        )
        .await?;
    let ids: BTreeSet<String> = rows
        .iter()
        .filter_map(|r| r["id"].as_str().map(String::from))
        .collect();
    let mut nodes: Vec<Value> = rows
        .into_iter()
        .map(|mut r| {
            let id = r["id"].as_str().unwrap_or_default().to_string();
            let h = health.get(&id);
            // history mode shows structure and what changed, not today's health (review M6 #21)
            r["health"] = if q.at.is_some() {
                json!("history")
            } else {
                h.map(|h| h["health"].clone()).unwrap_or(json!("uncovered"))
            };
            r["tests"] = h.map(|h| h["tests"].clone()).unwrap_or(json!(0));
            if q.at.is_some() && r["level"] == "symbol" {
                r["changed_since"] = json!(r["version_at"] != r["version"]);
            }
            r
        })
        .collect();
    let mut out_edges = Vec::new();
    for coll in ["calls", "depends_on", "member_of", "tested_by", "binds_to"] {
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
    // ghost nodes from open changesets (live view only)
    let open = if q.at.is_some() {
        vec![]
    } else {
        store.list_changesets(true).await?
    };
    let mut ghosts = Vec::new();
    let mut proposed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for cs in &open {
        let csk = cs["_key"].as_str().unwrap_or_default().to_string();
        if cs["state"] == "escalated" {
            continue;
        }
        let mut seen_new: BTreeSet<String> = BTreeSet::new();
        for (pk, p) in cs["proposals"].as_object().cloned().unwrap_or_default() {
            if p["new"] == json!(true) {
                seen_new.insert(p["display_name"].as_str().unwrap_or("").to_string());
                ghosts.push(json!({
                    "id": format!("ghost:{csk}:{pk}"), "key": pk, "level": "symbol", "ghost": true,
                    "label": p["display_name"], "kind": p["kind"], "language": p["language"],
                    "parent": p["component"].as_str().map(|c| format!("components/{}", key_of(c))),
                    "changeset": csk, "health": "ghost", "action": "add",
                }));
            } else {
                proposed
                    .entry(format!("symbols/{pk}"))
                    .or_default()
                    .push(csk.clone());
            }
        }
        for ch in cs["plan"]["changes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let name = ch["display_name"].as_str().unwrap_or("").to_string();
            let action = ch["action"].as_str().unwrap_or("modify");
            if action == "add" && !seen_new.contains(&name) {
                ghosts.push(json!({
                    "id": format!("ghost:{csk}:plan:{name}"), "key": name, "level": "symbol", "ghost": true,
                    "label": name, "kind": ch["kind"], "language": ch["language"],
                    "parent": ch["component"].as_str().map(|c| format!("components/{}", key_of(c))),
                    "changeset": csk, "health": "ghost", "action": "add", "description": ch["description"],
                }));
            } else if let Some(sym) = ch["symbol"].as_str() {
                proposed
                    .entry(format!("symbols/{}", key_of(sym)))
                    .or_default()
                    .push(csk.clone());
            }
        }
    }
    for n in nodes.iter_mut() {
        if let Some(cs) = n["id"].as_str().and_then(|id| proposed.get(id)) {
            let mut v = cs.clone();
            v.sort();
            v.dedup();
            n["proposed_in"] = json!(v);
        }
    }
    nodes.extend(ghosts);
    let mission = store
        .aql(
            "FOR m IN @@missions FILTER m.scope == 'program' LIMIT 1 RETURN m",
            json!({}),
        )
        .await?;
    Ok(Json(
        json!({"mission": mission.first(), "nodes": nodes, "edges": out_edges, "at": q.at}),
    ))
}

/// Merged changesets, releases and seed imports, oldest first — the time slider's stops.
pub async fn timeline(State(s): State<AppState>) -> ApiResult<Value> {
    let rows = s
        .store
        .aql(
            "LET seed = (FOR x IN @@symbols FILTER x.created_by == 'seed' COLLECT AGGREGATE t = MAX(x.created_at) RETURN t)
             LET merges = (FOR c IN @@changesets FILTER c.state == 'merged'
                           RETURN {kind: 'merge', id: c._id, key: c._key, label: c.title, at: c.merged_at,
                                   symbols: ATTRIBUTES(c.merged_versions || {})})
             LET rels = (FOR r IN @@releases RETURN {kind: 'release', id: r._id, key: r._key, label: r.name, at: r.created_at,
                                   symbols: []})
             LET s0 = seed[0] ? [{kind: 'seed', id: 'seed', key: 'seed', label: 'Seed imported', at: seed[0], symbols: []}] : []
             FOR e IN UNION(s0, merges, rels) SORT e.at RETURN e",
            json!({}),
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

/// Accept a symbol key or a display name.
pub async fn resolve_symbol(s: &AppState, name: &str) -> Result<String, ApiError> {
    let k = key_of(name);
    if s.store.db.get("symbols", k, None).await.is_ok() {
        return Ok(k.to_string());
    }
    let doc = s.store.symbol_by_name(None, k).await?;
    Ok(doc["_key"].as_str().unwrap_or_default().to_string())
}

/// Code and trace edges change what is built and what counts as tested, so through the API they
/// only change inside a changeset (`propose_edge`), never directly on main (final review #9).
fn guard_api_edge(collection: &str) -> Result<(), ApiError> {
    const CHANGESET_ONLY: &[&str] = &[
        "calls",
        "depends_on",
        "member_of",
        "tested_by",
        "binds_to",
        "verifies",
        "satisfies",
    ];
    if CHANGESET_ONLY.contains(&collection) {
        return Err(ApiError::bad_request(format!(
            "`{collection}` edges change code or coverage — propose them in a changeset (propose_edge)"
        )));
    }
    Ok(())
}
