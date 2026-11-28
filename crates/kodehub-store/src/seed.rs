//! YAML seed importer (`kodehub seed examples/auth-poc`). Idempotent: re-importing unchanged
//! data creates no new versions, documents or edges.
//!
//! Layout (docs/ontology.md names): `philosophy.yaml`; `requirements.yaml`, `contexts.yaml`,
//! `containers.yaml`, `components.yaml` (documents with a local `_key`); `code.yaml` and
//! `tests.yaml` (code and test nodes with their content); `edges.yaml`, a map from an edge
//! collection to `[from, to]` or `[from, to, {data}]` pairs of local ids (`code/x`,
//! `requirements/y`; `project` is the project's own node). After the edges, every placed node
//! is put into the current project version (`Store::sync_tree`).

use std::path::Path;

use kodehub_core::model::{Kind, Language, VersionContent, Visibility};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::store::{edge_key, NewSymbol, PROTECTED_EDGES};
use crate::{Result, Store, StoreError};

#[derive(Debug, Deserialize)]
struct SeedSymbol {
    key: String,
    display_name: String,
    kind: Kind,
    language: Language,
    #[serde(default)]
    visibility: Visibility,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    description: Option<String>,
    /// Namespace projection (`namespace`, `public_name`) used when rendering.
    #[serde(default)]
    namespace: Option<String>,
    #[serde(default)]
    public_name: Option<String>,
    #[serde(default)]
    signature: String,
    body: String,
    #[serde(default)]
    attributes: Option<Value>,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SeedReport {
    pub documents_created: usize,
    pub documents_updated: usize,
    pub symbols_created: usize,
    pub versions_created: usize,
    pub symbols_changed: usize,
    pub edges_created: usize,
}

/// Document collections loaded from `<collection>.yaml`, in dependency order.
const DOC_FILES: &[&str] = &["requirements", "contexts", "containers", "components"];

/// Edge collections a seed may write.
const SEED_EDGES: &[&str] = &[
    "contains",
    "member_of",
    "calls",
    "depends_on",
    "binds_to",
    "tested_with",
    "applies_to",
    "verified_by",
    "relates_to",
];

fn read_yaml(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| StoreError::Seed(format!("{}: {e}", path.display())))?;
    let v: Value = serde_yaml::from_str(&text)
        .map_err(|e| StoreError::Seed(format!("{}: {e}", path.display())))?;
    Ok(Some(v))
}

fn norm_id(store: &Store, s: &str) -> Result<String> {
    if s == "project" {
        return store.project_node();
    }
    store.db.qid(s)
}

pub async fn import_dir(store: &Store, dir: &Path) -> Result<SeedReport> {
    if !dir.is_dir() {
        return Err(StoreError::Seed(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    // seeding writes versions: serialise with acceptances and deployments
    let _write = store.write_lock.lock().await;
    let mut report = SeedReport::default();
    if let Some(v) = read_yaml(&dir.join("philosophy.yaml"))? {
        let text = v["text"]
            .as_str()
            .ok_or_else(|| StoreError::Seed("philosophy.yaml needs `text`".into()))?;
        let before = store.get_philosophy().await?;
        store
            .set_philosophy_with(text, "seed", v["title"].as_str(), None)
            .await?;
        if before.is_none() {
            report.documents_created += 1;
        } else if before
            .and_then(|b| b["text"].as_str().map(str::to_string))
            .as_deref()
            != Some(text.trim())
        {
            report.documents_updated += 1;
        }
    }
    for coll in DOC_FILES {
        let Some(v) = read_yaml(&dir.join(format!("{coll}.yaml")))? else {
            continue;
        };
        for doc in v.as_array().cloned().unwrap_or_default() {
            let key =
                store.db.pkey(doc["_key"].as_str().ok_or_else(|| {
                    StoreError::Seed(format!("{coll}: every document needs _key"))
                })?)?;
            let mut doc = doc.clone();
            doc["_key"] = json!(key);
            match store.db.get(coll, &key, None).await {
                Ok(existing) => {
                    let obj = doc.as_object().cloned().unwrap_or_default();
                    let differs = obj
                        .iter()
                        .any(|(k, v)| k != "_key" && existing.get(k) != Some(v));
                    if differs {
                        if *coll == "requirements" {
                            let mut patch = doc.clone();
                            patch.as_object_mut().unwrap().remove("_key");
                            let (d, _) = store
                                .update_requirement(&key, &patch, "seed", false)
                                .await?;
                            // seed data ships with its tests: covered as imported
                            store
                                .db
                                .patch(
                                    "requirements",
                                    &key,
                                    &json!({"covered_revision": d["revision"], "at_risk": false}),
                                    None,
                                )
                                .await?;
                        } else {
                            store.db.patch(coll, &key, &doc, None).await?;
                        }
                        report.documents_updated += 1;
                    }
                }
                Err(e) if e.is_not_found() => {
                    if *coll == "requirements" {
                        let mut d = doc.clone();
                        d["covered_revision"] = json!(1);
                        store.create_requirement(d).await?;
                    } else {
                        let mut d = doc.clone();
                        d["created_at"] = json!(kodehub_core::model::now_iso());
                        store.db.insert(coll, &d, None).await?;
                    }
                    report.documents_created += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    for file in ["code.yaml", "tests.yaml"] {
        let Some(v) = read_yaml(&dir.join(file))? else {
            continue;
        };
        let syms: Vec<SeedSymbol> =
            serde_json::from_value(v).map_err(|e| StoreError::Seed(format!("{file}: {e}")))?;
        for mut s in syms {
            if (file == "tests.yaml") != (s.kind == Kind::Test) {
                return Err(StoreError::Seed(format!(
                    "{file}: `{}` belongs in {}",
                    s.key,
                    if s.kind == Kind::Test {
                        "tests.yaml"
                    } else {
                        "code.yaml"
                    }
                )));
            }
            s.key = store.db.pkey(&s.key)?;
            let content = VersionContent {
                kind: s.kind,
                language: s.language,
                signature: s.signature.clone(),
                body: s.body.clone(),
                attributes: s.attributes.clone().unwrap_or_else(|| json!({})),
            };
            let coll = Store::node_collection(s.kind);
            match store.db.get(coll, &s.key, None).await {
                Ok(existing) => {
                    let placed = store
                        .tree(None)
                        .await?
                        .version(&format!("{coll}/{}", s.key))
                        .is_some();
                    if placed {
                        let out = store.write_version(&s.key, &content, "seed").await?;
                        if out.changed {
                            report.symbols_changed += 1;
                        }
                        if out.created_version {
                            report.versions_created += 1;
                        }
                    }
                    let mut patch = serde_json::Map::new();
                    for (k, v) in [
                        ("namespace", json!(s.namespace)),
                        ("public_name", json!(s.public_name)),
                    ] {
                        if existing.get(k).unwrap_or(&Value::Null) != &v {
                            patch.insert(k.into(), v);
                        }
                    }
                    if !patch.is_empty() {
                        store
                            .db
                            .patch(coll, &s.key, &Value::Object(patch), None)
                            .await?;
                        report.documents_updated += 1;
                    }
                }
                Err(e) if e.is_not_found() => {
                    let out = store
                        .create_symbol(
                            &NewSymbol {
                                key: Some(s.key.clone()),
                                display_name: s.display_name.clone(),
                                kind: s.kind,
                                language: s.language,
                                visibility: s.visibility,
                                tags: s.tags.clone(),
                                description: s.description.clone(),
                                parent: None,
                            },
                            &content,
                            "seed",
                        )
                        .await?;
                    if s.namespace.is_some() || s.public_name.is_some() {
                        store
                            .db
                            .patch(
                                coll,
                                &s.key,
                                &json!({"namespace": s.namespace, "public_name": s.public_name}),
                                None,
                            )
                            .await?;
                    }
                    report.symbols_created += 1;
                    if out.created_version {
                        report.versions_created += 1;
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    if let Some(v) = read_yaml(&dir.join("edges.yaml"))? {
        let map = v
            .as_object()
            .ok_or_else(|| StoreError::Seed("edges.yaml must be a map".into()))?;
        for (coll, list) in map {
            if PROTECTED_EDGES.contains(&coll.as_str()) || !SEED_EDGES.contains(&coll.as_str()) {
                return Err(StoreError::Seed(format!(
                    "edges.yaml may not seed `{coll}`"
                )));
            }
            for item in list.as_array().cloned().unwrap_or_default() {
                let arr = item.as_array().cloned().unwrap_or_default();
                if arr.len() < 2 {
                    return Err(StoreError::Seed(format!("{coll}: edge needs [from, to]")));
                }
                let from = norm_id(store, arr[0].as_str().unwrap_or_default())?;
                let to = norm_id(store, arr[1].as_str().unwrap_or_default())?;
                for id in [&from, &to] {
                    store
                        .get_node(id)
                        .await
                        .map_err(|_| StoreError::Seed(format!("{coll}: unknown node {id}")))?;
                }
                let key = edge_key(coll, &from, &to);
                let mut doc = arr
                    .get(2)
                    .cloned()
                    .filter(Value::is_object)
                    .unwrap_or(json!({}));
                doc["_key"] = json!(key);
                doc["_from"] = json!(from);
                doc["_to"] = json!(to);
                match store.db.insert(coll, &doc, None).await {
                    Ok(_) => report.edges_created += 1,
                    Err(e) if e.is_unique_violation() => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }
    // every placed node into the current project version
    store.sync_tree("seed").await?;
    // Union inferred references into the declared edges so a closure always contains every
    // symbol its code mentions (hand-written edges can be incomplete — review finding).
    let g = store.load_code_graph().await?;
    for s in g.symbols.values() {
        let Ok(v) = g.version(&s.version) else {
            continue;
        };
        let text = format!("{}\n{}", v.signature, v.body);
        for (to, kind) in kodehub_core::infer::references(&g, &s.id, s.language, &text) {
            let coll = kind.collection();
            let (f, t) = (
                crate::queries::node_id(&g, &s.id),
                crate::queries::node_id(&g, &to),
            );
            let doc =
                json!({"_key": edge_key(coll, &f, &t), "_from": f, "_to": t, "inferred": true});
            match store.db.insert(coll, &doc, None).await {
                Ok(_) => report.edges_created += 1,
                Err(e) if e.is_unique_violation() => {}
                Err(e) => return Err(e),
            }
        }
    }
    let problems = store.check_invariants().await?;
    if !problems.is_empty() {
        return Err(StoreError::Invariant(problems.join("; ")));
    }
    Ok(report)
}
