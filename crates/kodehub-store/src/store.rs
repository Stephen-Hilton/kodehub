//! Domain operations over the graph. Every reader/writer (server, CLI, agents) goes through here.

use std::collections::BTreeMap;

use kodehub_core::model::{
    now_iso, CodeEdgeKind, Kind, Language, VersionContent, Visibility, IMMUTABLE_COLLECTIONS,
};
use kodehub_core::{CodeEdge, CodeGraph, CodeSymbol};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::arango::{Arango, ArangoConfig};
use crate::{Result, StoreError};

/// Edge collections that may only be written by dedicated operations (versioning, releases,
/// deployments), never through the generic edge API.
pub const PROTECTED_EDGES: &[&str] = &[
    "has_version",
    "supports",
    "in_environment",
    "included_in",
    "deployed_to",
    "currently_running",
    "previously_ran",
    "proposes",
];

/// Document collections writable through the generic node API.
pub const GENERIC_NODE_COLLECTIONS: &[&str] = &[
    "missions",
    "contexts",
    "containers",
    "components",
    "namespaces",
    "requirement_groups",
    "build_targets",
];

const CODE_EDGES: &[&str] = &["calls", "depends_on", "member_of", "tested_by", "binds_to"];

#[derive(Debug, Clone)]
pub struct Store {
    pub db: Arango,
    /// Serialises merges and deployments within this server process (review M6 #3).
    pub write_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSymbol {
    #[serde(default)]
    pub key: Option<String>,
    pub display_name: String,
    pub kind: Kind,
    pub language: Language,
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WriteOutcome {
    pub symbol: String,
    pub version: String,
    /// A new version document was created (false when the content already existed).
    pub created_version: bool,
    /// The symbol's stable version changed.
    pub changed: bool,
}

/// Deterministic edge key so re-imports and repeated proposals are idempotent.
pub fn edge_key(coll: &str, from: &str, to: &str) -> String {
    blake3::hash(format!("{coll}|{from}|{to}").as_bytes()).to_hex()[..24].to_string()
}

pub fn key_of(id: &str) -> &str {
    id.split_once('/').map(|x| x.1).unwrap_or(id)
}

pub fn new_key(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    )
}

/// Slug suitable for an ArangoDB key.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

impl Store {
    pub fn new(cfg: ArangoConfig) -> Self {
        Store {
            db: Arango::new(cfg),
            write_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Connect, bootstrap the registry and select the active project (creating one if the
    /// database has none). A pre-projects database is copied into a project but kept.
    pub async fn open(cfg: ArangoConfig) -> Result<Self> {
        Self::open_with(cfg, None).await
    }

    /// Like `open`; with `backup_dir`, the old pre-projects collections are backed up there as
    /// JSON and dropped after migration.
    pub async fn open_with(
        cfg: ArangoConfig,
        backup_dir: Option<&std::path::Path>,
    ) -> Result<Self> {
        let s = Store::new(cfg);
        s.select_initial_project(backup_dir).await?;
        Ok(s)
    }

    pub async fn aql(&self, q: &str, bind: Value) -> Result<Vec<Value>> {
        self.db.aql(q, bind, None).await
    }

    pub async fn health(&self) -> Value {
        match self.db.version().await {
            Ok(v) => {
                json!({"status": "ok", "db": {"connected": true, "version": v["version"], "database": self.db.cfg.database, "project": self.db.project()}})
            }
            Err(e) => {
                json!({"status": "degraded", "db": {"connected": false, "error": e.to_string()}})
            }
        }
    }

    // ---------------------------------------------------------------- generic graph access

    pub async fn get_node(&self, id: &str) -> Result<Value> {
        let (coll, key) = id
            .split_once('/')
            .ok_or_else(|| StoreError::Invalid(format!("node id must be collection/key: {id}")))?;
        self.db.get(coll, key, None).await
    }

    /// Query a collection with simple equality filters.
    pub async fn query_nodes(
        &self,
        collection: &str,
        filter: &serde_json::Map<String, Value>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        validate_name(collection)?;
        let mut clauses = Vec::new();
        let mut bind = serde_json::Map::new();
        bind.insert("@c".into(), json!(collection));
        bind.insert("limit".into(), json!(limit));
        for (i, (k, v)) in filter.iter().enumerate() {
            validate_name(k)?;
            clauses.push(format!("FILTER d.@f{i} == @v{i}"));
            bind.insert(format!("f{i}"), json!(k));
            bind.insert(format!("v{i}"), v.clone());
        }
        let q = format!(
            "FOR d IN @@c {} SORT d._key LIMIT @limit RETURN d",
            clauses.join(" ")
        );
        self.aql(&q, Value::Object(bind)).await
    }

    /// Traverse from `start` over `edges` (empty = whole named graph) in a direction.
    pub async fn traverse(
        &self,
        start: &str,
        direction: &str,
        edges: &[String],
        depth: u32,
    ) -> Result<Value> {
        let dir = match direction.to_ascii_lowercase().as_str() {
            "outbound" | "out" => "OUTBOUND",
            "inbound" | "in" => "INBOUND",
            "any" => "ANY",
            d => return Err(StoreError::Invalid(format!("bad direction {d}"))),
        };
        for e in edges {
            validate_name(e)?;
        }
        let over = if edges.is_empty() {
            "GRAPH @kodehub_graph".to_string()
        } else {
            edges
                .iter()
                .map(|e| self.db.phys(e))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let depth = depth.clamp(1, 20);
        let q = format!(
            "FOR v, e IN 1..{depth} {dir} @start {over} OPTIONS {{uniqueVertices: 'global', order: 'bfs'}}
             RETURN {{vertex: v, edge: e}}"
        );
        let rows = self.aql(&q, json!({"start": start})).await?;
        let vertices: Vec<Value> = rows.iter().map(|r| r["vertex"].clone()).collect();
        let edges: Vec<Value> = rows.iter().map(|r| r["edge"].clone()).collect();
        Ok(json!({"start": start, "vertices": vertices, "edges": edges}))
    }

    pub async fn insert_node(&self, collection: &str, mut doc: Value) -> Result<Value> {
        if !GENERIC_NODE_COLLECTIONS.contains(&collection) {
            return Err(StoreError::Invalid(format!(
                "collection `{collection}` cannot be written through the generic node API"
            )));
        }
        if doc.get("_key").is_none() {
            let base = doc
                .get("title")
                .or_else(|| doc.get("name"))
                .and_then(Value::as_str)
                .map(slug)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| new_key(collection));
            doc["_key"] = json!(base);
        }
        doc["created_at"] = json!(now_iso());
        let created = self.db.insert(collection, &doc, None).await?;
        if crate::c4::C4_COLLECTIONS.contains(&collection) {
            let key = created["_key"].as_str().unwrap_or_default().to_string();
            self.sync_c4_version(collection, &key, "api").await?;
        }
        Ok(created)
    }

    /// Patch a document. Immutable collections always refuse.
    pub async fn update_node(&self, collection: &str, key: &str, patch: &Value) -> Result<Value> {
        if collection == "symbols" && patch.get("visibility").is_some() {
            // visibility changes rendered code → closure hashes must move with it (final review #9)
            let _w = self.write_lock.lock().await;
            let v = self.update_node_inner(collection, key, patch).await?;
            self.refresh_closure_hashes(None).await?;
            return Ok(v);
        }
        self.update_node_inner(collection, key, patch).await
    }

    async fn update_node_inner(&self, collection: &str, key: &str, patch: &Value) -> Result<Value> {
        if IMMUTABLE_COLLECTIONS.contains(&collection) {
            return Err(StoreError::Immutable(format!(
                "documents in `{collection}` are immutable"
            )));
        }
        if !GENERIC_NODE_COLLECTIONS.contains(&collection) && collection != "symbols" {
            return Err(StoreError::Invalid(format!(
                "collection `{collection}` cannot be patched through the generic node API"
            )));
        }
        if collection == "symbols" {
            // Only presentation fields are editable on symbols.
            if let Some(obj) = patch.as_object() {
                if let Some(bad) = obj
                    .keys()
                    .find(|k| !["tags", "description", "visibility"].contains(&k.as_str()))
                {
                    return Err(StoreError::Invalid(format!(
                        "symbol field `{bad}` cannot be patched directly"
                    )));
                }
            }
        }
        let out = self.db.patch(collection, key, patch, None).await?;
        if crate::c4::C4_COLLECTIONS.contains(&collection) {
            self.sync_c4_version(collection, key, "api").await?;
        }
        Ok(out)
    }

    /// Insert an edge through the generic API (protected edge collections refuse).
    pub async fn insert_edge(
        &self,
        collection: &str,
        from: &str,
        to: &str,
        data: Value,
    ) -> Result<Value> {
        check_edge_collection(collection)?;
        self.get_node(from).await?;
        self.get_node(to).await?;
        let mut doc = if data.is_object() { data } else { json!({}) };
        doc["_key"] = json!(edge_key(collection, from, to));
        doc["_from"] = json!(from);
        doc["_to"] = json!(to);
        self.db.insert_ignore(collection, &doc, None).await?;
        if CODE_EDGES.contains(&collection) {
            self.refresh_closure_hashes(None).await?;
        }
        self.db
            .get(collection, &edge_key(collection, from, to), None)
            .await
    }

    pub async fn remove_edge(&self, collection: &str, from: &str, to: &str) -> Result<()> {
        check_edge_collection(collection)?;
        self.aql(
            "FOR e IN @@c FILTER e._from == @f AND e._to == @t REMOVE e IN @@c",
            json!({"@c": collection, "f": from, "t": to}),
        )
        .await?;
        if CODE_EDGES.contains(&collection) {
            self.refresh_closure_hashes(None).await?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- code: symbols & versions

    fn version_doc(content: &VersionContent, created_by: &str) -> (String, Value) {
        let n = content.normalized();
        let hash = n.hash();
        let doc = json!({
            "_key": hash,
            "content_hash": hash,
            "kind": n.kind,
            "language": n.language,
            "signature": n.signature,
            "body": n.body,
            "attributes": n.attributes,
            "created_at": now_iso(),
            "created_by": created_by,
        });
        (hash, doc)
    }

    async fn insert_version(&self, doc: &Value, trx: &str) -> Result<bool> {
        match self.db.insert("versions", doc, Some(trx)).await {
            Ok(_) => Ok(true),
            Err(e) if e.is_unique_violation() => Ok(false),
            Err(e) => Err(e),
        }
    }

    const VERSION_WRITE: &'static [&'static str] = &["versions", "has_version", "symbols"];

    /// The stable `has_version` edge of a node (`symbols/x`, `components/y`, …), if any.
    pub async fn stable_edge_in(&self, node_id: &str, trx: Option<&str>) -> Result<Option<Value>> {
        Ok(self
            .db
            .aql(
                "FOR e IN @@has_version FILTER e._from == @n AND e.state == 'stable' RETURN e",
                json!({"n": node_id}),
                trx,
            )
            .await?
            .into_iter()
            .next())
    }

    /// Key of a node's stable version, if any.
    pub async fn stable_version_in(
        &self,
        node_id: &str,
        trx: Option<&str>,
    ) -> Result<Option<String>> {
        Ok(self
            .stable_edge_in(node_id, trx)
            .await?
            .and_then(|e| e["_to"].as_str().map(|t| key_of(t).to_string())))
    }

    async fn next_seq(&self, node_id: &str, trx: Option<&str>) -> Result<i64> {
        Ok(self
            .db
            .aql(
                "RETURN MAX(FOR e IN @@has_version FILTER e._from == @n RETURN e.seq) || 0",
                json!({"n": node_id}),
                trx,
            )
            .await?
            .first()
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1)
    }

    /// Promote `version` to be the node's stable version (spec §5.2): close the old stable edge
    /// (`validTo` = now, state retired) and open the new one (`validFrom` = now), turning a
    /// matching candidate edge into the stable one when it exists. The unique sparse index on
    /// `stable_of` makes a second stable edge impossible.
    pub async fn promote_in(
        &self,
        node_id: &str,
        version_coll: &str,
        version: &str,
        changeset: Option<&str>,
        by: &str,
        trx: &str,
    ) -> Result<i64> {
        let now = now_iso();
        let vid = format!("{version_coll}/{version}");
        if let Some(old) = self.stable_edge_in(node_id, Some(trx)).await? {
            if old["_to"] == json!(vid) {
                return Ok(old["seq"].as_i64().unwrap_or(0));
            }
            self.db
                .patch(
                    "has_version",
                    old["_key"].as_str().unwrap_or_default(),
                    &json!({"state": "retired", "validTo": now, "stable_of": null, "retired_by": version}),
                    Some(trx),
                )
                .await?;
        }
        let cand = match changeset {
            Some(cs) => self
                .db
                .aql(
                    "FOR e IN @@has_version FILTER e._from == @n AND e._to == @v
                       AND e.state == 'candidate' AND e.changeset == @cs RETURN e",
                    json!({"n": node_id, "v": vid, "cs": cs}),
                    Some(trx),
                )
                .await?
                .into_iter()
                .next(),
            None => None,
        };
        let seq = match cand {
            Some(c) => {
                self.db
                    .patch(
                        "has_version",
                        c["_key"].as_str().unwrap_or_default(),
                        &json!({"state": "stable", "validFrom": now, "validTo": null,
                                "stable_of": node_id, "promoted_by": by}),
                        Some(trx),
                    )
                    .await?;
                c["seq"].as_i64().unwrap_or(0)
            }
            None => {
                let seq = self.next_seq(node_id, Some(trx)).await?;
                self.db
                    .insert(
                        "has_version",
                        &json!({"_from": node_id, "_to": vid, "seq": seq, "state": "stable",
                                "validFrom": now, "validTo": null, "stable_of": node_id,
                                "changeset": changeset, "created_at": now, "created_by": by}),
                        Some(trx),
                    )
                    .await
                    .map_err(|e| {
                        if e.is_unique_violation() {
                            StoreError::Invariant(format!("{node_id} already has a stable version"))
                        } else {
                            e
                        }
                    })?;
                seq
            }
        };
        Ok(seq)
    }

    /// Record a candidate version for an existing node: an edge with no `validFrom`, so it never
    /// disturbs the stable version (spec §5.2). Idempotent per (node, version, changeset).
    pub async fn add_candidate(
        &self,
        node_id: &str,
        version_coll: &str,
        version: &str,
        changeset: &str,
        by: &str,
    ) -> Result<i64> {
        let vid = format!("{version_coll}/{version}");
        // one candidate per node per changeset: a re-proposal replaces the previous candidate
        self.db
            .aql(
                "FOR e IN @@has_version FILTER e._from == @n AND e.state == 'candidate'
                   AND e.changeset == @cs AND e._to != @v REMOVE e IN @@has_version",
                json!({"n": node_id, "cs": changeset, "v": vid}),
                None,
            )
            .await?;
        if let Some(e) = self
            .db
            .aql(
                "FOR e IN @@has_version FILTER e._from == @n AND e._to == @v
                   AND e.state == 'candidate' AND e.changeset == @cs RETURN e",
                json!({"n": node_id, "v": vid, "cs": changeset}),
                None,
            )
            .await?
            .into_iter()
            .next()
        {
            return Ok(e["seq"].as_i64().unwrap_or(0));
        }
        let seq = self.next_seq(node_id, None).await?;
        self.db
            .insert(
                "has_version",
                &json!({"_key": edge_key("has_version", node_id, &format!("{vid}|{changeset}")),
                        "_from": node_id, "_to": vid, "seq": seq, "state": "candidate",
                        "validFrom": null, "validTo": null, "stable_of": null,
                        "changeset": changeset, "created_at": now_iso(), "created_by": by}),
                None,
            )
            .await?;
        Ok(seq)
    }

    /// Drop the candidate edges of a changeset (withdrawn or rejected proposals).
    pub async fn discard_candidates(&self, changeset: &str, node_id: Option<&str>) -> Result<()> {
        self.db
            .aql(
                "FOR e IN @@has_version FILTER e.state == 'candidate' AND e.changeset == @cs
                   AND (@n == null OR e._from == @n)
                   UPDATE e WITH {state: 'discarded'} IN @@has_version",
                json!({"cs": changeset, "n": node_id}),
                None,
            )
            .await
            .map(|_| ())
    }

    /// The version a node had stable at time `at` (RFC 3339), from the validity intervals.
    pub async fn version_as_of(&self, node_id: &str, at: &str) -> Result<Option<Value>> {
        Ok(self
            .aql(
                "FOR e IN @@has_version FILTER e._from == @n AND e.validFrom != null
                   AND e.validFrom <= @at AND (e.validTo == null OR e.validTo > @at)
                   RETURN {version: PARSE_IDENTIFIER(e._to).key, seq: e.seq, validFrom: e.validFrom, validTo: e.validTo}",
                json!({"n": node_id, "at": at}),
            )
            .await?
            .into_iter()
            .next())
    }

    /// Create a symbol with its first version, in one transaction.
    pub async fn create_symbol(
        &self,
        sym: &NewSymbol,
        content: &VersionContent,
        created_by: &str,
    ) -> Result<WriteOutcome> {
        if content.kind != sym.kind || content.language != sym.language {
            return Err(StoreError::Invalid(
                "version kind/language must match the symbol".into(),
            ));
        }
        let key = sym.key.clone().unwrap_or_else(|| {
            format!(
                "{}-{}",
                sym.language.as_str(),
                slug(&sym.display_name.replace("::", "-").replace('.', "-"))
            )
        });
        let (hash, vdoc) = Self::version_doc(content, created_by);
        let trx = self.db.begin(Self::VERSION_WRITE, &[]).await?;
        let res: Result<bool> = async {
            let sdoc = json!({
                "_key": key,
                "display_name": sym.display_name,
                "kind": sym.kind,
                "language": sym.language,
                "visibility": sym.visibility,
                "tags": sym.tags,
                "description": sym.description,
                "created_at": now_iso(),
                "created_by": created_by,
            });
            self.db
                .insert("symbols", &sdoc, Some(&trx))
                .await
                .map_err(|e| {
                    if e.is_unique_violation() {
                        StoreError::Conflict(format!(
                            "a {} symbol named `{}` (or key `{key}`) already exists",
                            sym.language.as_str(),
                            sym.display_name
                        ))
                    } else {
                        e
                    }
                })?;
            let created = self.insert_version(&vdoc, &trx).await?;
            self.promote_in(
                &format!("symbols/{key}"),
                "versions",
                &hash,
                None,
                created_by,
                &trx,
            )
            .await?;
            self.refresh_closure_hashes(Some(&trx)).await?;
            Ok(created)
        }
        .await;
        match res {
            Ok(created) => {
                self.db.commit(&trx).await?;
                Ok(WriteOutcome {
                    symbol: key,
                    version: hash,
                    created_version: created,
                    changed: true,
                })
            }
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                Err(e)
            }
        }
    }

    /// The versioning rule (doc 02): reuse identical content, move current → previous, create
    /// the new current edge — all in one transaction.
    pub async fn write_version(
        &self,
        symbol_key: &str,
        content: &VersionContent,
        created_by: &str,
    ) -> Result<WriteOutcome> {
        let trx = self.db.begin(Self::VERSION_WRITE, &[]).await?;
        let res = self
            .write_version_in(symbol_key, content, created_by, &trx, true)
            .await;
        match res {
            Ok(o) => {
                self.db.commit(&trx).await?;
                Ok(o)
            }
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                Err(e)
            }
        }
    }

    /// Versioning rule inside an existing transaction.
    pub async fn write_version_in(
        &self,
        symbol_key: &str,
        content: &VersionContent,
        created_by: &str,
        trx: &str,
        refresh: bool,
    ) -> Result<WriteOutcome> {
        let sym = self.db.get("symbols", symbol_key, Some(trx)).await?;
        if sym["kind"] != json!(content.kind) || sym["language"] != json!(content.language) {
            return Err(StoreError::Invalid(format!(
                "version kind/language must match symbol `{symbol_key}`"
            )));
        }
        let (hash, vdoc) = Self::version_doc(content, created_by);
        let sym_id = format!("symbols/{symbol_key}");
        let current = self.stable_version_in(&sym_id, Some(trx)).await?;
        if current.is_none() {
            return Err(StoreError::Invariant(format!(
                "symbol {symbol_key} has no stable version"
            )));
        }
        if current.as_deref() == Some(hash.as_str()) {
            return Ok(WriteOutcome {
                symbol: symbol_key.into(),
                version: hash,
                created_version: false,
                changed: false,
            });
        }
        let created = self.insert_version(&vdoc, trx).await?;
        self.promote_in(&sym_id, "versions", &hash, None, created_by, trx)
            .await?;
        if refresh {
            self.refresh_closure_hashes(Some(trx)).await?;
        }
        Ok(WriteOutcome {
            symbol: symbol_key.into(),
            version: hash,
            created_version: created,
            changed: true,
        })
    }

    /// Attempt to add a second stable `has_version` edge directly. Exists so the invariant can
    /// be demonstrated and tested; the database refuses it when the node already has one.
    pub async fn force_add_stable_version(&self, version: &str, symbol_key: &str) -> Result<()> {
        let sid = format!("symbols/{symbol_key}");
        self.db
            .insert(
                "has_version",
                &json!({"_from": sid, "_to": format!("versions/{version}"), "seq": 0,
                        "state": "stable", "validFrom": now_iso(), "validTo": null, "stable_of": sid}),
                None,
            )
            .await
            .map(|_| ())
            .map_err(|e| {
                if e.is_unique_violation() {
                    StoreError::Invariant(format!(
                        "symbol {symbol_key} already has a stable version"
                    ))
                } else {
                    e
                }
            })
    }

    /// Load the code graph (current versions) for closure computation and rendering.
    pub async fn load_code_graph_in(&self, trx: Option<&str>) -> Result<CodeGraph> {
        let syms = self
            .db
            .aql(
                "FOR s IN @@symbols
                   LET cv = FIRST(FOR e IN @@has_version FILTER e._from == s._id AND e.state == 'stable' RETURN PARSE_IDENTIFIER(e._to).key)
                   FILTER cv != null
                   RETURN {id: s._key, display_name: s.display_name, kind: s.kind, language: s.language,
                           visibility: s.visibility, tags: s.tags || [], version: cv}",
                json!({}),
                trx,
            )
            .await?;
        let mut g = CodeGraph::default();
        // planned symbols (no stable version yet) are not part of the code graph
        for s in syms {
            let cs: CodeSymbol = serde_json::from_value(s)
                .map_err(|e| StoreError::Invalid(format!("bad symbol doc: {e}")))?;
            g.symbols.insert(cs.id.clone(), cs);
        }
        for coll in CODE_EDGES {
            let rows = self
                .db
                .aql(
                    "FOR e IN @@c FILTER STARTS_WITH(e._from, CONCAT(@pfx, 'symbols/')) AND STARTS_WITH(e._to, CONCAT(@pfx, 'symbols/'))
                     RETURN {from: PARSE_IDENTIFIER(e._from).key, to: PARSE_IDENTIFIER(e._to).key, pinned_version: e.pinned_version}",
                    json!({"@c": coll}),
                    trx,
                )
                .await?;
            for r in rows {
                g.edges.push(CodeEdge {
                    from: r["from"].as_str().unwrap_or_default().into(),
                    to: r["to"].as_str().unwrap_or_default().into(),
                    kind: CodeEdgeKind::from_collection(coll).expect("code edge"),
                    pinned_version: r["pinned_version"].as_str().map(String::from),
                });
            }
        }
        g.edges.sort();
        let mut needed: Vec<String> = g.symbols.values().map(|s| s.version.clone()).collect();
        needed.extend(g.edges.iter().filter_map(|e| e.pinned_version.clone()));
        needed.sort();
        needed.dedup();
        let versions = self
            .db
            .aql(
                "FOR v IN @@versions FILTER v._key IN @keys RETURN v",
                json!({"keys": needed}),
                trx,
            )
            .await?;
        for v in versions {
            let key = v["_key"].as_str().unwrap_or_default().to_string();
            g.versions.insert(key, version_content_from_doc(&v)?);
        }
        Ok(g)
    }

    pub async fn load_code_graph(&self) -> Result<CodeGraph> {
        self.load_code_graph_in(None).await
    }

    /// Load arbitrary versions by hash.
    pub async fn load_versions(&self, keys: &[String]) -> Result<BTreeMap<String, VersionContent>> {
        let rows = self
            .aql(
                "FOR v IN @@versions FILTER v._key IN @keys RETURN v",
                json!({"keys": keys}),
            )
            .await?;
        rows.iter()
            .map(|v| {
                Ok((
                    v["_key"].as_str().unwrap_or_default().to_string(),
                    version_content_from_doc(v)?,
                ))
            })
            .collect()
    }

    /// Recompute closure hashes for every symbol and store the changed ones.
    pub async fn refresh_closure_hashes(&self, trx: Option<&str>) -> Result<usize> {
        let g = self.load_code_graph_in(trx).await?;
        // A pin conflict in one closure must not block every write: that symbol's closure hash
        // becomes null (its tests show as not run) until the conflict is resolved.
        let pairs: Vec<Value> = g
            .symbols
            .keys()
            .map(|k| json!({"k": k, "h": g.closure_hash(k).ok()}))
            .collect();
        let updated = self
            .db
            .aql(
                "FOR p IN @pairs LET s = DOCUMENT(@@symbols, p.k)
                   FILTER s.closure_hash != p.h
                   UPDATE p.k WITH {closure_hash: p.h} IN @@symbols RETURN 1",
                json!({"pairs": pairs}),
                trx,
            )
            .await?;
        Ok(updated.len())
    }

    /// Invariant report. Empty means healthy.
    pub async fn check_invariants(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let rows = self
            .aql(
                "FOR n IN UNION(
                    (FOR s IN @@symbols RETURN s._id), (FOR c IN @@components RETURN c._id),
                    (FOR c IN @@containers RETURN c._id), (FOR c IN @@contexts RETURN c._id))
                   LET k = LENGTH(FOR e IN @@has_version FILTER e._from == n AND e.state == 'stable' RETURN 1)
                   FILTER k > 1 RETURN {n, k}",
                json!({}),
            )
            .await?;
        for r in rows {
            problems.push(format!(
                "{} has {} stable versions (expected at most 1)",
                r["n"], r["k"]
            ));
        }
        let rows = self
            .aql(
                "FOR s IN @@symbols
                   LET k = LENGTH(FOR e IN @@has_version FILTER e._from == s._id AND e.state == 'stable' RETURN 1)
                   FILTER k == 0 AND s.lifecycle != 'planned' RETURN s._key",
                json!({}),
            )
            .await?;
        for r in rows {
            problems.push(format!(
                "symbol {r} has no stable version and is not planned"
            ));
        }
        let rows = self
            .aql(
                "FOR e1 IN @@has_version FILTER e1.validFrom != null
                   FOR e2 IN @@has_version FILTER e2._from == e1._from AND e2._key > e1._key AND e2.validFrom != null
                     FILTER e1.validFrom < (e2.validTo || '9999') AND e2.validFrom < (e1.validTo || '9999')
                     RETURN {n: e1._from, a: e1.seq, b: e2.seq}",
                json!({}),
            )
            .await?;
        for r in rows {
            problems.push(format!(
                "{} has overlapping validity for versions {} and {}",
                r["n"], r["a"], r["b"]
            ));
        }
        let rows = self
            .aql(
                "FOR env IN @@environments LET n = LENGTH(FOR e IN @@currently_running FILTER e._from == env._id RETURN 1)
                   FILTER n > 1 RETURN {env: env._key, n}",
                json!({}),
            )
            .await?;
        for r in rows {
            problems.push(format!(
                "environment {} has {} currently_running edges (expected at most 1)",
                r["env"], r["n"]
            ));
        }
        Ok(problems)
    }

    /// Version history of a symbol, newest first.
    pub async fn version_history(&self, symbol_key: &str) -> Result<Vec<Value>> {
        self.aql(
            "LET sid = CONCAT(@pfx, 'symbols/', @k)
             FOR e IN @@has_version FILTER e._from == sid AND e.state IN ['stable', 'retired', 'candidate']
               SORT e.seq DESC
               RETURN MERGE(UNSET(DOCUMENT(e._to), '_id', '_rev'),
                            {seq: e.seq, state: e.state, current: e.state == 'stable',
                             since: e.validFrom, until: e.validTo, changeset: e.changeset})",
            json!({"k": symbol_key}),
        )
        .await
    }

    /// Namespaces with their exposed symbols: (name, [(symbol key, public name)]).
    pub async fn namespaces(&self) -> Result<Vec<(String, Vec<(String, String)>)>> {
        let rows = self
            .aql(
                "FOR n IN @@namespaces SORT n.name
                 RETURN {name: n.name, exposes: (FOR e IN @@exposes FILTER e._from == n._id
                          RETURN [PARSE_IDENTIFIER(e._to).key, e.public_name])}",
                json!({}),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let ex = r["exposes"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string())))
                    .collect();
                (r["name"].as_str().unwrap_or_default().to_string(), ex)
            })
            .collect())
    }

    pub async fn symbol_by_name(&self, language: Option<Language>, name: &str) -> Result<Value> {
        let rows = self
            .aql(
                "FOR s IN @@symbols FILTER (s.display_name == @n OR s._key == @n) AND (@l == null OR s.language == @l)
                 RETURN s",
                json!({"n": name, "l": language}),
            )
            .await?;
        match rows.len() {
            0 => Err(StoreError::NotFound(format!("symbol `{name}`"))),
            1 => Ok(rows[0].clone()),
            _ => Err(StoreError::Invalid(format!(
                "symbol name `{name}` is ambiguous across languages; pass a language"
            ))),
        }
    }

    // ---------------------------------------------------------------- requirements & decisions

    pub async fn create_requirement(&self, mut doc: Value) -> Result<Value> {
        if doc
            .get("statement")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
        {
            return Err(StoreError::Invalid("requirement needs a statement".into()));
        }
        if doc.get("_key").is_none() {
            doc["_key"] = json!(new_key("req"));
        }
        for f in ["constraints", "examples", "edge_cases", "out_of_scope"] {
            if doc.get(f).is_none() {
                doc[f] = json!([]);
            }
        }
        let kind = doc["kind"].as_str().unwrap_or("technical").to_string();
        if kind.is_empty()
            || kind.len() > 32
            || !kind.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        {
            return Err(StoreError::Invalid(
                "requirement kind is a lower-case word (business, technical, …)".into(),
            ));
        }
        doc["kind"] = json!(kind);
        doc["revision"] = json!(1);
        doc["at_risk"] = json!(false);
        // nothing is covered until a merged changeset brings a verifying test (final review #7)
        if doc.get("covered_revision").is_none() {
            doc["covered_revision"] = json!(0);
        }
        doc["created_at"] = json!(now_iso());
        doc["updated_at"] = doc["created_at"].clone();
        self.db.insert("requirements", &doc, None).await
    }

    /// Edit a requirement: snapshot the prior revision into `requirement_history`, bump
    /// `revision`. Returns the new document. A patch that changes nothing is a no-op.
    pub async fn update_requirement(
        &self,
        key: &str,
        patch: &Value,
        edited_by: &str,
        mark_at_risk: bool,
    ) -> Result<(Value, bool)> {
        const EDITABLE: &[&str] = &[
            "title",
            "statement",
            "intent",
            "constraints",
            "examples",
            "edge_cases",
            "out_of_scope",
            "kind",
        ];
        let obj = patch
            .as_object()
            .ok_or_else(|| StoreError::Invalid("patch must be an object".into()))?;
        if let Some(bad) = obj.keys().find(|k| !EDITABLE.contains(&k.as_str())) {
            return Err(StoreError::Invalid(format!(
                "requirement field `{bad}` is not editable"
            )));
        }
        let trx = self
            .db
            .begin(&["requirements", "requirement_history"], &[])
            .await?;
        let res: Result<(Value, bool)> = async {
            let cur = self.db.get("requirements", key, Some(&trx)).await?;
            let changed = obj.iter().any(|(k, v)| cur.get(k) != Some(v));
            if !changed {
                return Ok((cur, false));
            }
            let rev = cur["revision"].as_i64().unwrap_or(1);
            let mut snapshot = cur.clone();
            for f in ["_id", "_rev"] {
                snapshot.as_object_mut().unwrap().remove(f);
            }
            self.db
                .insert(
                    "requirement_history",
                    &json!({
                        "requirement": key,
                        "revision": rev,
                        "snapshot": snapshot,
                        "superseded_at": now_iso(),
                        "superseded_by": edited_by,
                    }),
                    Some(&trx),
                )
                .await?;
            let mut p = patch.clone();
            p["revision"] = json!(rev + 1);
            p["updated_at"] = json!(now_iso());
            p["updated_by"] = json!(edited_by);
            if mark_at_risk {
                p["at_risk"] = json!(true);
            }
            let new = self.db.patch("requirements", key, &p, Some(&trx)).await?;
            Ok((new, true))
        }
        .await;
        match res {
            Ok(r) => {
                self.db.commit(&trx).await?;
                Ok(r)
            }
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                Err(e)
            }
        }
    }

    pub async fn set_requirement_risk(&self, key: &str, at_risk: bool) -> Result<()> {
        self.db
            .patch("requirements", key, &json!({"at_risk": at_risk}), None)
            .await
            .map(|_| ())
    }

    pub async fn requirement_history(&self, key: &str) -> Result<Vec<Value>> {
        self.aql(
            "FOR h IN @@requirement_history FILTER h.requirement == @k SORT h.revision DESC RETURN h",
            json!({"k": key}),
        )
        .await
    }

    pub async fn create_decision(&self, mut doc: Value, justifies: &[String]) -> Result<Value> {
        if doc
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
        {
            return Err(StoreError::Invalid("decision needs a title".into()));
        }
        if doc.get("_key").is_none() {
            doc["_key"] = json!(new_key("dec"));
        }
        if doc.get("made_by").is_none() {
            doc["made_by"] = json!("human");
        }
        if doc.get("alternatives").is_none() {
            doc["alternatives"] = json!([]);
        }
        doc["created_at"] = json!(now_iso());
        let d = self.db.insert("decisions", &doc, None).await?;
        let did = d["_id"].as_str().unwrap_or_default().to_string();
        for from in justifies {
            self.insert_edge("justified_by", from, &did, json!({}))
                .await?;
        }
        Ok(d)
    }
}

pub fn version_content_from_doc(v: &Value) -> Result<VersionContent> {
    Ok(VersionContent {
        kind: serde_json::from_value(v["kind"].clone())
            .map_err(|e| StoreError::Invalid(format!("version kind: {e}")))?,
        language: serde_json::from_value(v["language"].clone())
            .map_err(|e| StoreError::Invalid(format!("version language: {e}")))?,
        signature: v["signature"].as_str().unwrap_or_default().into(),
        body: v["body"].as_str().unwrap_or_default().into(),
        attributes: if v["attributes"].is_object() {
            v["attributes"].clone()
        } else {
            json!({})
        },
    })
}

/// Only real edge collections that are not protected may be written through generic APIs.
pub fn check_edge_collection(collection: &str) -> Result<()> {
    if PROTECTED_EDGES.contains(&collection) {
        return Err(StoreError::Invalid(format!(
            "edges in `{collection}` are written only by dedicated operations"
        )));
    }
    if !kodehub_core::model::EDGE_COLLECTIONS.contains(&collection) {
        return Err(StoreError::Invalid(format!(
            "`{collection}` is not an edge collection"
        )));
    }
    Ok(())
}

fn validate_name(s: &str) -> Result<()> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(StoreError::Invalid(format!("invalid name `{s}`")));
    }
    Ok(())
}
