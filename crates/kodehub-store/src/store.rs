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
    "based_on",
    "owns",
    "has_work_item",
    "has_deployment",
    "has_deployer",
    "uses",
    "logs_to",
    "build_of",
    "built_for",
    "has_message",
    "authored",
];

/// Document collections writable through the generic node API.
pub const GENERIC_NODE_COLLECTIONS: &[&str] = &["contexts", "containers", "components"];

/// Presentation fields of code and test nodes that may be patched directly (their content is
/// only ever changed through versions).
pub const CODE_PRESENTATION_FIELDS: &[&str] = &[
    "tags",
    "description",
    "visibility",
    "namespace",
    "public_name",
];

pub const CODE_EDGES: &[&str] = &[
    "calls",
    "depends_on",
    "member_of",
    "tested_with",
    "binds_to",
];

/// Cached version trees: project version id → (tree revision, index).
pub type TreeCache = std::sync::Arc<
    std::sync::Mutex<
        std::collections::HashMap<String, (i64, std::sync::Arc<crate::tree::TreeIndex>)>,
    >,
>;

#[derive(Debug, Clone)]
pub struct Store {
    pub db: Arango,
    /// Serialises merges and deployments within this server process (review M6 #3).
    pub write_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    /// Version trees by project version and revision (ids are qualified, so one cache serves
    /// every project).
    pub trees: TreeCache,
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
    /// The component (or container / context, for tests) that holds the node.
    #[serde(default)]
    pub parent: Option<String>,
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

/// Deterministic edge key so re-imports and repeated proposals are idempotent. Qualified with
/// the project of `from` (a project node id, or `projects/<id>`), else of `to` (an edge from a
/// global record such as a work item).
pub fn edge_key(coll: &str, from: &str, to: &str) -> String {
    let h = blake3::hash(format!("{coll}|{from}|{to}").as_bytes()).to_hex()[..24].to_string();
    match id_project(from).or_else(|| id_project(to)) {
        Some(p) => crate::arango::qualify(&p, &h),
        None => h,
    }
}

/// The project an id belongs to: `code/shop.x` → `shop`, `projects/shop` → `shop`.
pub fn id_project(id: &str) -> Option<String> {
    let (coll, key) = id.split_once('/')?;
    if coll == crate::schema::PROJECTS || coll == "projects" {
        return Some(key.to_string());
    }
    crate::arango::key_project(key).map(str::to_string)
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
            trees: Default::default(),
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
        let id = &self.db.qid(id)?;
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
        let start = &self.db.qid(start)?;
        for e in edges {
            validate_name(e)?;
        }
        let over = if edges.is_empty() {
            "GRAPH @kodehub_graph".to_string()
        } else {
            edges
                .iter()
                .map(|e| e.to_string())
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
            doc["_key"] = json!(self.db.pkey(&base)?);
        } else if let Some(k) = doc["_key"].as_str() {
            doc["_key"] = json!(self.db.pkey(k)?);
        }
        doc["created_at"] = json!(now_iso());
        let created = self.db.insert(collection, &doc, None).await?;
        if crate::c4::C4_COLLECTIONS.contains(&collection) {
            let key = created["_key"].as_str().unwrap_or_default().to_string();
            self.sync_c4_version(collection, &key, "api").await?;
        }
        Ok(created)
    }

    /// Patch a document. Immutable collections always refuse. (Closure hashes are computed per
    /// project version when needed, so a visibility change moves them automatically.)
    pub async fn update_node(&self, collection: &str, key: &str, patch: &Value) -> Result<Value> {
        self.update_node_inner(collection, key, patch).await
    }

    async fn update_node_inner(&self, collection: &str, key: &str, patch: &Value) -> Result<Value> {
        if IMMUTABLE_COLLECTIONS.contains(&collection) {
            return Err(StoreError::Immutable(format!(
                "documents in `{collection}` are immutable"
            )));
        }
        let leaf = collection == "code" || collection == "tests";
        if !GENERIC_NODE_COLLECTIONS.contains(&collection) && !leaf {
            return Err(StoreError::Invalid(format!(
                "collection `{collection}` cannot be patched through the generic node API"
            )));
        }
        if leaf {
            // only presentation fields are editable on code and test nodes
            if let Some(obj) = patch.as_object() {
                if let Some(bad) = obj
                    .keys()
                    .find(|k| !CODE_PRESENTATION_FIELDS.contains(&k.as_str()))
                {
                    return Err(StoreError::Invalid(format!(
                        "field `{bad}` of a code node cannot be patched directly"
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
        let (from, to) = (&self.db.qid(from)?, &self.db.qid(to)?);
        self.get_node(from).await?;
        self.get_node(to).await?;
        let mut doc = if data.is_object() { data } else { json!({}) };
        doc["_key"] = json!(edge_key(collection, from, to));
        doc["_from"] = json!(from);
        doc["_to"] = json!(to);
        self.db.insert_ignore(collection, &doc, None).await?;
        // placing a node in the hierarchy puts it into the current project version
        if collection == "contains" {
            self.place_node(to, "api").await?;
        }
        self.db
            .get(collection, &edge_key(collection, from, to), None)
            .await
    }

    /// Put a placed node into the current project version: a C4 node with its definition, a
    /// code or test node with its newest version.
    pub async fn place_node(&self, id: &str, by: &str) -> Result<()> {
        let (coll, key) = id
            .split_once('/')
            .ok_or_else(|| StoreError::Invalid(format!("bad node id {id}")))?;
        if crate::c4::C4_COLLECTIONS.contains(&coll) {
            self.sync_c4_version(coll, key, by).await?;
        } else if coll == "code" || coll == "tests" {
            let newest = self
                .aql(
                    "FOR e IN @@has_version FILTER e._from == @n AND e.state != 'discarded'
                       AND e.work_item == null SORT e.seq DESC LIMIT 1 RETURN e._to",
                    json!({"n": id}),
                )
                .await?;
            if let Some(v) = newest.first().and_then(Value::as_str) {
                let mut ch = crate::tree::TreeChanges::default();
                ch.leaves.insert(id.to_string(), Some(v.to_string()));
                self.apply_tree_changes(None, &ch, by, None).await?;
            }
        }
        Ok(())
    }

    pub async fn remove_edge(&self, collection: &str, from: &str, to: &str) -> Result<()> {
        check_edge_collection(collection)?;
        let (from, to) = (&self.db.qid(from)?, &self.db.qid(to)?);
        self.aql(
            "FOR e IN @@c FILTER e._from == @f AND e._to == @t REMOVE e IN @@c",
            json!({"@c": collection, "f": from, "t": to}),
        )
        .await?;
        Ok(())
    }

    // ---------------------------------------------------------------- code and tests (D-057)

    /// The collection holding nodes of this kind: `tests` for tests, `code` for the rest.
    pub fn node_collection(kind: Kind) -> &'static str {
        if kind == Kind::Test {
            "tests"
        } else {
            "code"
        }
    }

    /// The id of a code or test node from its key or id (local or qualified).
    pub async fn symbol_id(&self, key_or_id: &str) -> Result<String> {
        if let Some((c, _)) = key_or_id.split_once('/') {
            if c == "code" || c == "tests" {
                return self.db.qid(key_or_id);
            }
        }
        let k = self.db.pkey(key_of(key_or_id))?;
        for c in ["code", "tests"] {
            match self.db.get(c, &k, None).await {
                Ok(_) => return Ok(format!("{c}/{k}")),
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(e),
            }
        }
        Err(StoreError::NotFound(format!("code or test `{key_or_id}`")))
    }

    /// A leaf version document of `node` (key derived from the node and the content hash).
    pub(crate) fn leaf_version_doc(
        &self,
        node_id: &str,
        content: &VersionContent,
        created_by: &str,
    ) -> Result<(String, Value)> {
        let n = content.normalized();
        let chash = n.hash();
        let project = self.db.require_project()?;
        let key = crate::tree::version_key(&project, node_id, &chash);
        let coll =
            crate::tree::version_collection(node_id.split_once('/').map(|x| x.0).unwrap_or(""))
                .ok_or_else(|| {
                    StoreError::Invalid(format!("{node_id} is not a code or test node"))
                })?;
        let doc = json!({
            "_key": key,
            "node": node_id,
            "content_hash": chash,
            "kind": n.kind,
            "language": n.language,
            "signature": n.signature,
            "body": n.body,
            "attributes": n.attributes,
            "created_at": now_iso(),
            "created_by": created_by,
        });
        Ok((format!("{coll}/{key}"), doc))
    }

    pub(crate) async fn next_seq(&self, node_id: &str, trx: Option<&str>) -> Result<i64> {
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

    /// Write a version of a code or test node with its `has_version` edge. `state` is
    /// `candidate` (a work item's proposal, or written but not yet in any tree) or `accepted`.
    /// A work item has at most one candidate per node: a newer one replaces the older.
    /// Returns (version id, whether the version document was created).
    pub async fn put_leaf_version(
        &self,
        node_id: &str,
        content: &VersionContent,
        by: &str,
        work_item: Option<&str>,
        trx: Option<&str>,
    ) -> Result<(String, bool)> {
        let node_id = &self.db.qid(node_id)?;
        let (coll, key) = node_id
            .split_once('/')
            .ok_or_else(|| StoreError::Invalid(format!("bad node id {node_id}")))?;
        let node = self.db.get(coll, key, trx).await?;
        if node["kind"] != json!(content.kind) || node["language"] != json!(content.language) {
            return Err(StoreError::Invalid(format!(
                "version kind/language must match `{node_id}`"
            )));
        }
        let (vid, doc) = self.leaf_version_doc(node_id, content, by)?;
        let vcoll = vid.split_once('/').map(|x| x.0).unwrap_or_default();
        let created = match self.db.insert(vcoll, &doc, trx).await {
            Ok(_) => true,
            Err(e) if e.is_unique_violation() => false,
            Err(e) => return Err(e),
        };
        if let Some(w) = work_item {
            self.db
                .aql(
                    "FOR e IN @@has_version FILTER e._from == @n AND e.state == 'candidate'
                       AND e.work_item == @w AND e._to != @v
                       UPDATE e WITH {state: 'discarded', discarded_at: @now} IN @@has_version",
                    json!({"n": node_id, "w": w, "v": vid, "now": now_iso()}),
                    trx,
                )
                .await?;
        }
        let ekey = edge_key(
            "has_version",
            node_id,
            &format!("{vid}|{}", work_item.unwrap_or("")),
        );
        let exists = self
            .db
            .aql(
                "FOR e IN @@has_version FILTER e._key == @k RETURN e.state",
                json!({"k": ekey}),
                trx,
            )
            .await?;
        if exists.is_empty() {
            let seq = self.next_seq(node_id, trx).await?;
            self.db
                .insert(
                    "has_version",
                    &json!({"_key": ekey, "_from": node_id, "_to": vid, "seq": seq,
                            "state": "candidate", "work_item": work_item,
                            "created_at": now_iso(), "created_by": by}),
                    trx,
                )
                .await?;
        } else if exists[0] == "discarded" {
            self.db
                .patch("has_version", &ekey, &json!({"state": "candidate"}), trx)
                .await?;
        }
        Ok((vid, created))
    }

    /// Mark a work item's candidate versions discarded (withdrawn or rejected proposals).
    pub async fn discard_candidates(&self, work_item: &str, node_id: Option<&str>) -> Result<()> {
        let node_id = node_id.map(|n| self.db.qid(n)).transpose()?;
        self.db
            .aql(
                "FOR e IN @@has_version FILTER e.state == 'candidate' AND e.work_item == @w
                   AND (@n == null OR e._from == @n)
                   UPDATE e WITH {state: 'discarded', discarded_at: @now} IN @@has_version",
                json!({"w": work_item, "n": node_id, "now": now_iso()}),
                None,
            )
            .await
            .map(|_| ())
    }

    /// Create a code or test node with its first version. With `parent` (a component, or a
    /// container / context for tests), the node is placed and the version accepted into the
    /// current project version at once; otherwise it waits for [`Store::sync_tree`].
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
        let key = self.db.pkey(&sym.key.clone().unwrap_or_else(|| {
            format!(
                "{}-{}",
                sym.language.as_str(),
                slug(&sym.display_name.replace("::", "-").replace('.', "-"))
            )
        }))?;
        let coll = Self::node_collection(sym.kind);
        // display names are unique per language across code and tests (one code graph)
        let clash = self
            .aql(
                "RETURN UNION(
                   (FOR s IN @@code FILTER s.language == @l AND (s.display_name == @n OR s._key == @k) RETURN 1),
                   (FOR s IN @@tests FILTER s.language == @l AND (s.display_name == @n OR s._key == @k) RETURN 1))",
                json!({"l": sym.language, "n": sym.display_name, "k": key}),
            )
            .await?;
        if clash
            .first()
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
        {
            return Err(StoreError::Conflict(format!(
                "a {} symbol named `{}` (or key `{key}`) already exists",
                sym.language.as_str(),
                sym.display_name
            )));
        }
        let node_id = format!("{coll}/{key}");
        self.db
            .insert(
                coll,
                &json!({
                    "_key": key,
                    "display_name": sym.display_name,
                    "kind": sym.kind,
                    "language": sym.language,
                    "visibility": sym.visibility,
                    "tags": sym.tags,
                    "description": sym.description,
                    "lifecycle": null,
                    "created_at": now_iso(),
                    "created_by": created_by,
                }),
                None,
            )
            .await
            .map_err(|e| {
                if e.is_unique_violation() {
                    StoreError::Conflict(format!("`{key}` already exists"))
                } else {
                    e
                }
            })?;
        let (vid, created) = self
            .put_leaf_version(&node_id, content, created_by, None, None)
            .await?;
        if let Some(parent) = &sym.parent {
            let parent = self.db.qid(parent)?;
            self.get_node(&parent).await?;
            self.db
                .insert_ignore(
                    "contains",
                    &json!({"_key": edge_key("contains", &parent, &node_id), "_from": parent,
                            "_to": node_id}),
                    None,
                )
                .await?;
            let mut ch = crate::tree::TreeChanges::default();
            ch.leaves.insert(node_id.clone(), Some(vid.clone()));
            self.apply_tree_changes(None, &ch, created_by, None).await?;
        }
        Ok(WriteOutcome {
            symbol: key,
            version: key_of(&vid).to_string(),
            created_version: created,
            changed: true,
        })
    }

    /// Write a new version of a placed node straight into the current project version (seed,
    /// import, tests and maintenance; agents go through work items). Identical content is a
    /// no-op.
    pub async fn write_version(
        &self,
        symbol_key: &str,
        content: &VersionContent,
        created_by: &str,
    ) -> Result<WriteOutcome> {
        let node_id = self.symbol_id(symbol_key).await?;
        let pv = self.current_version().await?;
        let tree = self.tree(Some(&pv)).await?;
        let (vid, doc) = self.leaf_version_doc(&node_id, content, created_by)?;
        let _ = doc;
        if tree.version(&node_id) == Some(vid.as_str()) {
            return Ok(WriteOutcome {
                symbol: key_of(&node_id).to_string(),
                version: key_of(&vid).to_string(),
                created_version: false,
                changed: false,
            });
        }
        let (vid, created) = self
            .put_leaf_version(&node_id, content, created_by, None, None)
            .await?;
        let mut ch = crate::tree::TreeChanges::default();
        ch.leaves.insert(node_id.clone(), Some(vid.clone()));
        self.apply_tree_changes(Some(&pv), &ch, created_by, None)
            .await?;
        Ok(WriteOutcome {
            symbol: key_of(&node_id).to_string(),
            version: key_of(&vid).to_string(),
            created_version: created,
            changed: true,
        })
    }

    /// Bring the current project version in line with the identity graph: place every node
    /// that has a parent but is not in the tree yet (code and tests with their newest version,
    /// C4 nodes with their definition), and give C4 nodes whose definition changed a new
    /// version. Used after a seed or an import. Returns how many nodes changed.
    pub async fn sync_tree(&self, by: &str) -> Result<usize> {
        let pv = self.current_version().await?;
        let tree = self.tree(Some(&pv)).await?;
        let mut ch = crate::tree::TreeChanges::default();
        for coll in crate::tree::LEAF_LEVELS {
            let rows = self
                .aql(
                    "FOR n IN @@c
                       LET placed = LENGTH(FOR e IN @@contains FILTER e._to == n._id RETURN 1) > 0
                       FILTER placed
                       LET v = FIRST(FOR e IN @@has_version FILTER e._from == n._id AND e.state != 'discarded'
                                       AND e.work_item == null SORT e.seq DESC LIMIT 1 RETURN e._to)
                       FILTER v != null
                       RETURN {n: n._id, v}",
                    json!({"@c": coll}),
                )
                .await?;
            for r in rows {
                let (Some(n), Some(v)) = (r["n"].as_str(), r["v"].as_str()) else {
                    continue;
                };
                if tree.version(n) != Some(v) {
                    ch.leaves.insert(n.to_string(), Some(v.to_string()));
                }
            }
        }
        for coll in crate::tree::C4_LEVELS {
            let rows = self
                .aql(
                    "FOR n IN @@c
                       LET placed = LENGTH(FOR e IN @@contains FILTER e._to == n._id RETURN 1) > 0
                       FILTER placed RETURN n",
                    json!({"@c": coll}),
                )
                .await?;
            for n in rows {
                let id = n["_id"].as_str().unwrap_or_default().to_string();
                let def = crate::c4::c4_definition(coll, &n);
                let hash = crate::c4::c4_hash(&def);
                let current = tree
                    .version(&id)
                    .and_then(|v| tree.content_hash.get(v))
                    .cloned();
                if current.as_deref() != Some(hash.as_str()) {
                    ch.defs.insert(id, def);
                }
            }
        }
        let n = ch.leaves.len() + ch.defs.len();
        self.apply_tree_changes(Some(&pv), &ch, by, None).await?;
        Ok(n)
    }

    /// Load the code graph of a project version (`None` = current) for closures, rendering
    /// and testing: every code and test node in its tree at the version the tree holds.
    pub async fn load_code_graph_at(&self, pv: Option<&str>) -> Result<CodeGraph> {
        let tree = self.tree(pv).await?;
        self.code_graph_of(&tree, None).await
    }

    /// The code graph of a resolved tree (inside `trx` when given, e.g. to check a change
    /// before committing it).
    pub async fn code_graph_of(
        &self,
        tree: &crate::tree::TreeIndex,
        trx: Option<&str>,
    ) -> Result<CodeGraph> {
        let mut pairs: Vec<Value> = Vec::new();
        for coll in crate::tree::LEAF_LEVELS {
            for n in tree.nodes_in(coll) {
                pairs.push(json!({"n": n, "v": tree.version(n)}));
            }
        }
        let rows = self
            .db
            .aql(
                "FOR p IN @pairs LET s = DOCUMENT(p.n) LET v = DOCUMENT(p.v)
                   RETURN {s: {id: s._key, display_name: s.display_name, kind: s.kind,
                               language: s.language, visibility: s.visibility, tags: s.tags || [],
                               version: v._key},
                           v: v}",
                json!({"pairs": pairs}),
                trx,
            )
            .await?;
        let mut g = CodeGraph::default();
        for r in rows {
            let cs: CodeSymbol = serde_json::from_value(r["s"].clone())
                .map_err(|e| StoreError::Invalid(format!("bad code node: {e}")))?;
            g.versions
                .insert(cs.version.clone(), version_content_from_doc(&r["v"])?);
            g.symbols.insert(cs.id.clone(), cs);
        }
        for coll in CODE_EDGES {
            let rows = self
                .db
                .aql(
                    "FOR e IN @@c FILTER PARSE_IDENTIFIER(e._from).collection IN ['code', 'tests']
                       AND PARSE_IDENTIFIER(e._to).collection IN ['code', 'tests']
                     RETURN {from: PARSE_IDENTIFIER(e._from).key, to: PARSE_IDENTIFIER(e._to).key,
                             pinned_version: e.pinned_version}",
                    json!({"@c": coll}),
                    trx,
                )
                .await?;
            for r in rows {
                let (from, to) = (
                    r["from"].as_str().unwrap_or_default().to_string(),
                    r["to"].as_str().unwrap_or_default().to_string(),
                );
                // edges to nodes outside this project version (planned, removed) are not part
                // of its graph
                if !g.symbols.contains_key(&from) || !g.symbols.contains_key(&to) {
                    continue;
                }
                g.edges.push(CodeEdge {
                    from,
                    to,
                    kind: CodeEdgeKind::from_collection(coll).expect("code edge"),
                    pinned_version: r["pinned_version"].as_str().map(|p| key_of(p).to_string()),
                });
            }
        }
        g.edges.sort();
        let pinned: Vec<String> = g
            .edges
            .iter()
            .filter_map(|e| e.pinned_version.clone())
            .filter(|p| !g.versions.contains_key(p))
            .collect();
        if !pinned.is_empty() {
            for (k, v) in self.load_versions(&pinned).await? {
                g.versions.insert(k, v);
            }
        }
        Ok(g)
    }

    /// The current project version's code graph.
    pub async fn load_code_graph(&self) -> Result<CodeGraph> {
        self.load_code_graph_at(None).await
    }

    /// Load code and test versions by key or id.
    pub async fn load_versions(&self, keys: &[String]) -> Result<BTreeMap<String, VersionContent>> {
        let keys: Vec<String> = keys.iter().map(|k| key_of(k).to_string()).collect();
        let rows = self
            .aql(
                "FOR v IN UNION((FOR x IN @@code_versions FILTER x._key IN @keys RETURN x),
                                (FOR x IN @@test_versions FILTER x._key IN @keys RETURN x))
                 RETURN v",
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

    /// Invariant report for the current project version. Empty means healthy.
    pub async fn check_invariants(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let pv = self.current_version().await?;
        // a node appears at most once in a tree (hard rule 1, D-057)
        let rows = self
            .aql(
                "FOR v IN 1..8 OUTBOUND @pv @@contains
                   COLLECT node = v.node WITH COUNT INTO n FILTER n > 1 RETURN {node, n}",
                json!({"pv": pv}),
            )
            .await?;
        for r in rows {
            problems.push(format!(
                "{} appears {} times in {pv} (expected at most once)",
                r["node"], r["n"]
            ));
        }
        // every version in the tree is a version of its node
        let rows = self
            .aql(
                "FOR v IN 1..8 OUTBOUND @pv @@contains
                   LET ok = LENGTH(FOR e IN @@has_version FILTER e._from == v.node AND e._to == v._id RETURN 1) > 0
                   FILTER !ok RETURN v._id",
                json!({"pv": pv}),
            )
            .await?;
        for r in rows {
            problems.push(format!("{r} is in the tree but not a version of its node"));
        }
        // every C4 version lists exactly the children its edges hold
        let rows = self
            .aql(
                "FOR v IN 1..7 OUTBOUND @pv @@contains
                   FILTER v.children != null
                   LET kids = (FOR c IN 1..1 OUTBOUND v @@contains SORT c._id RETURN c._id)
                   FILTER kids != v.children RETURN v._id",
                json!({"pv": pv}),
            )
            .await?;
        for r in rows {
            problems.push(format!("{r}: child edges differ from its children list"));
        }
        Ok(problems)
    }

    /// Versions of a code or test node, newest first, with the project versions holding each.
    pub async fn version_history(&self, symbol_key: &str) -> Result<Vec<Value>> {
        let node = self.symbol_id(symbol_key).await?;
        let rows = self
            .aql(
                "FOR e IN @@has_version FILTER e._from == @n SORT e.seq DESC
                   RETURN MERGE(UNSET(DOCUMENT(e._to), '_id', '_rev'),
                                {seq: e.seq, state: e.state, work_item: e.work_item,
                                 version_id: e._to, created_at: e.created_at})",
                json!({"n": node}),
            )
            .await?;
        let cur = self.tree(None).await?.version(&node).map(str::to_string);
        let mut out = Vec::new();
        let mut in_versions: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for pv in self.list_project_versions().await? {
            let id = pv["_id"].as_str().unwrap_or_default().to_string();
            if let Some(v) = self.tree(Some(&id)).await?.version(&node) {
                in_versions
                    .entry(v.to_string())
                    .or_default()
                    .push(pv["name"].as_str().unwrap_or_default().to_string());
            }
        }
        for mut r in rows {
            let vid = r["version_id"].as_str().unwrap_or_default().to_string();
            r["current"] = json!(cur.as_deref() == Some(vid.as_str()));
            r["project_versions"] = json!(in_versions.get(&vid).cloned().unwrap_or_default());
            out.push(r);
        }
        Ok(out)
    }

    /// Record a test execution (or, with `kind: run`, an ad-hoc run) as an immutable
    /// `test_logs` entry, with `logs_to` edges from the version it ran (`test_version` /
    /// `version` key) and from the build that produced its artifact, when known.
    pub async fn record_test_log(&self, mut doc: Value, build: Option<&str>) -> Result<Value> {
        if doc.get("kind").is_none() {
            doc["kind"] = json!("test");
        }
        if doc.get("ran_at").is_none() {
            doc["ran_at"] = json!(now_iso());
        }
        if let Some(k) = doc.get("_key").and_then(Value::as_str) {
            doc["_key"] = json!(self.db.pkey(k)?);
        }
        let created = self.db.insert("test_logs", &doc, None).await?;
        let id = created["_id"].as_str().unwrap_or_default().to_string();
        let from = if doc["kind"] == "run" {
            doc["version"]
                .as_str()
                .map(|v| format!("code_versions/{}", key_of(v)))
        } else {
            doc["test_version"]
                .as_str()
                .map(|v| format!("test_versions/{}", key_of(v)))
        };
        for f in from.into_iter().chain(build.map(str::to_string)) {
            if self.get_node(&f).await.is_ok() {
                self.db
                    .insert_ignore(
                        "logs_to",
                        &json!({"_key": edge_key("logs_to", &f, &id), "_from": f, "_to": id}),
                        None,
                    )
                    .await?;
            }
        }
        Ok(created)
    }

    /// Namespaces with their exposed symbols: (name, [(symbol key, public name)]). The
    /// projection lives on the code nodes (`namespace`, `public_name`).
    pub async fn namespaces(&self) -> Result<Vec<(String, Vec<(String, String)>)>> {
        let rows = self
            .aql(
                "FOR s IN @@code FILTER s.namespace != null
                   COLLECT ns = s.namespace INTO g
                   SORT ns
                   RETURN {name: ns, exposes: (FOR x IN g SORT x.s._key
                            RETURN [x.s._key, x.s.public_name || x.s.display_name])}",
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
                "FOR s IN UNION((FOR x IN @@code RETURN x), (FOR x IN @@tests RETURN x))
                   FILTER (s.display_name == @n OR s._key == @k) AND (@l == null OR s.language == @l)
                 RETURN s",
                json!({"n": name, "k": self.db.pkey(name)?, "l": language}),
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

    /// Requirement fields that are versioned content (the rest is identity and bookkeeping).
    pub const REQUIREMENT_CONTENT: &'static [&'static str] = &[
        "title",
        "statement",
        "intent",
        "constraints",
        "examples",
        "edge_cases",
        "out_of_scope",
        "kind",
        "tags",
        "rationale",
        "alternatives",
    ];

    fn requirement_content(doc: &Value) -> Value {
        let mut c = serde_json::Map::new();
        for f in Self::REQUIREMENT_CONTENT {
            if let Some(v) = doc.get(*f) {
                c.insert((*f).into(), v.clone());
            }
        }
        Value::Object(c)
    }

    /// Write a requirement version (immutable) with its `has_version` edge.
    #[allow(clippy::too_many_arguments)]
    async fn put_requirement_version(
        &self,
        req_id: &str,
        revision: i64,
        content: &Value,
        state: &str,
        by: &str,
        extra: Value,
        trx: Option<&str>,
    ) -> Result<Value> {
        let key = self.db.pkey(&format!(
            "{}-r{revision}-{}",
            crate::arango::local_key(key_of(req_id)),
            &uuid::Uuid::new_v4().simple().to_string()[..6]
        ))?;
        let mut doc = json!({
            "_key": key, "requirement": req_id, "revision": revision, "state": state,
            "content": content, "content_hash": kodehub_core::canonical::hash_value(content),
            "created_at": now_iso(), "created_by": by,
        });
        if let (Some(d), Some(e)) = (doc.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                d.insert(k.clone(), v.clone());
            }
        }
        let v = self.db.insert("requirement_versions", &doc, trx).await?;
        let vid = v["_id"].as_str().unwrap_or_default().to_string();
        self.db
            .insert(
                "has_version",
                &json!({"_key": edge_key("has_version", req_id, &vid), "_from": req_id, "_to": vid,
                        "seq": revision,
                        "state": if state == "accepted" { "accepted" } else if state == "proposed" { "candidate" } else { "discarded" },
                        "created_at": now_iso(), "created_by": by}),
                trx,
            )
            .await?;
        Ok(v)
    }

    /// `put_requirement_version` for other modules (governance).
    pub(crate) async fn put_requirement_version_pub(
        &self,
        req_id: &str,
        revision: i64,
        doc: &Value,
        state: &str,
        by: &str,
        extra: Value,
    ) -> Result<Value> {
        self.put_requirement_version(
            req_id,
            revision,
            &Self::requirement_content(doc),
            state,
            by,
            extra,
            None,
        )
        .await
    }

    /// Create a requirement: its node (holding the current content for fast reads) and its
    /// first version. `state: proposed` creates one waiting for a human (see `governance`).
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
            doc["_key"] = json!(self.db.pkey(&new_key("req"))?);
        } else if let Some(k) = doc["_key"].as_str() {
            doc["_key"] = json!(self.db.pkey(k)?);
        }
        for f in [
            "constraints",
            "examples",
            "edge_cases",
            "out_of_scope",
            "tags",
        ] {
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
                "requirement kind is a lower-case word (business, technical, decision, …)".into(),
            ));
        }
        doc["kind"] = json!(kind);
        doc["revision"] = json!(1);
        doc["at_risk"] = json!(false);
        if doc.get("state").is_none() {
            doc["state"] = json!("accepted");
        }
        // nothing is covered until an accepted proposal brings a verifying test (final review #7)
        if doc.get("covered_revision").is_none() {
            doc["covered_revision"] = json!(0);
        }
        doc["created_at"] = json!(now_iso());
        doc["updated_at"] = doc["created_at"].clone();
        let by = doc["created_by"].as_str().unwrap_or("human").to_string();
        let created = self
            .db
            .insert("requirements", &doc, None)
            .await
            .map_err(|e| {
                if e.is_unique_violation() {
                    StoreError::Conflict(format!("requirement `{}` exists", doc["_key"]))
                } else {
                    e
                }
            })?;
        let id = created["_id"].as_str().unwrap_or_default().to_string();
        let state = doc["state"].as_str().unwrap_or("accepted").to_string();
        self.put_requirement_version(
            &id,
            1,
            &Self::requirement_content(&doc),
            &state,
            &by,
            json!({}),
            None,
        )
        .await?;
        self.own(&id).await?;
        Ok(created)
    }

    /// Edit a requirement: a new accepted version (revision + 1) and the node's current
    /// content updated. Returns the new document. A patch that changes nothing is a no-op.
    pub async fn update_requirement(
        &self,
        key: &str,
        patch: &Value,
        edited_by: &str,
        mark_at_risk: bool,
    ) -> Result<(Value, bool)> {
        let obj = patch
            .as_object()
            .ok_or_else(|| StoreError::Invalid("patch must be an object".into()))?;
        if let Some(bad) = obj
            .keys()
            .find(|k| !Self::REQUIREMENT_CONTENT.contains(&k.as_str()))
        {
            return Err(StoreError::Invalid(format!(
                "requirement field `{bad}` is not editable"
            )));
        }
        let key = &self.db.pkey(key_of(key))?;
        let trx = self
            .db
            .begin(
                &["requirements", "requirement_versions", "has_version"],
                &[],
            )
            .await?;
        let res: Result<(Value, bool)> = async {
            let cur = self.db.get("requirements", key, Some(&trx)).await?;
            let changed = obj.iter().any(|(k, v)| cur.get(k) != Some(v));
            if !changed {
                return Ok((cur, false));
            }
            let rev = cur["revision"].as_i64().unwrap_or(1) + 1;
            let mut next = cur.clone();
            for (k, v) in obj {
                next[k] = v.clone();
            }
            let id = cur["_id"].as_str().unwrap_or_default().to_string();
            self.put_requirement_version(
                &id,
                rev,
                &Self::requirement_content(&next),
                "accepted",
                edited_by,
                json!({}),
                Some(&trx),
            )
            .await?;
            let mut p = patch.clone();
            p["revision"] = json!(rev);
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

    /// Every version of a requirement, newest first (accepted, proposed and rejected).
    pub async fn requirement_history(&self, key: &str) -> Result<Vec<Value>> {
        self.aql(
            "FOR v IN @@requirement_versions FILTER v.requirement == CONCAT('requirements/', @k)
               SORT v.revision DESC, v.created_at DESC
               RETURN MERGE(v, {snapshot: v.content})",
            json!({"k": self.db.pkey(key_of(key))?}),
        )
        .await
    }

    /// Record a decision: a requirement of kind `decision` (docs/ontology.md), applying to the
    /// nodes it justifies.
    pub async fn create_decision(&self, doc: Value, justifies: &[String]) -> Result<Value> {
        let title = doc
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if title.is_empty() {
            return Err(StoreError::Invalid("decision needs a title".into()));
        }
        let mut r = json!({
            "kind": "decision",
            "title": title,
            "statement": doc["choice"].as_str().or(doc["statement"].as_str()).unwrap_or(&title),
            "rationale": doc["rationale"],
            "alternatives": doc.get("alternatives").cloned().unwrap_or(json!([])),
            "made_by": doc.get("made_by").cloned().unwrap_or(json!("human")),
            "tags": doc.get("tags").cloned().unwrap_or(json!([])),
            "covered_revision": 1,
        });
        if let Some(k) = doc.get("_key") {
            r["_key"] = k.clone();
        } else {
            r["_key"] = json!(self.db.pkey(&new_key("dec"))?);
        }
        let d = self.create_requirement(r).await?;
        let did = d["_id"].as_str().unwrap_or_default().to_string();
        for n in justifies {
            self.insert_edge("applies_to", &did, n, json!({})).await?;
        }
        Ok(d)
    }

    /// Decisions (requirements of kind `decision`) with the nodes they apply to.
    pub async fn list_decisions(&self) -> Result<Vec<Value>> {
        self.aql(
            "FOR r IN @@requirements FILTER r.kind == 'decision' SORT r.created_at DESC
               RETURN MERGE(r, {choice: r.statement,
                                justifies: (FOR e IN @@applies_to FILTER e._from == r._id RETURN e._to)})",
            json!({}),
        )
        .await
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
