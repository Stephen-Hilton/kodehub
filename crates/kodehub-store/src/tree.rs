//! The version tree (D-057, docs/ontology.md "The version tree").
//!
//! Unversioned nodes (`contexts`, `containers`, `components`, `code`, `tests`) are stable
//! anchors. Their content lives in immutable version nodes, and every C4 version lists **all**
//! of its child versions through `contains` edges written once with it. A **project version**
//! is the root: while open its `contains` edges move as work is accepted (with a
//! compare-and-set on its `tree_rev`); once frozen it never changes.
//!
//! "The version of node N" always means "in project version P": the version of N reachable
//! from P. [`TreeIndex`] answers that for a whole project version in one walk.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use kodehub_core::model::now_iso;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::arango::qualify;
use crate::store::edge_key;
use crate::{Result, Store, StoreError};

/// Unversioned node collections in the C4 hierarchy, top first.
pub const C4_LEVELS: &[&str] = &["contexts", "containers", "components"];
/// Leaf node collections (their versions hold content, not children).
pub const LEAF_LEVELS: &[&str] = &["code", "tests"];
/// Every collection holding versions in the tree.
pub const VERSION_COLLECTIONS: &[&str] = &[
    "project_versions",
    "context_versions",
    "container_versions",
    "component_versions",
    "code_versions",
    "test_versions",
];

/// The version collection of a node collection (`code` → `code_versions`).
pub fn version_collection(node_coll: &str) -> Option<&'static str> {
    Some(match node_coll {
        "projects" => "project_versions",
        "contexts" => "context_versions",
        "containers" => "container_versions",
        "components" => "component_versions",
        "code" => "code_versions",
        "tests" => "test_versions",
        "requirements" => "requirement_versions",
        _ => return None,
    })
}

/// Rank of a node collection in the hierarchy (lower = higher up).
pub fn level_rank(node_coll: &str) -> usize {
    match node_coll {
        "projects" => 0,
        "contexts" => 1,
        "containers" => 2,
        "components" => 3,
        _ => 4,
    }
}

fn coll_of(id: &str) -> &str {
    id.split_once('/').map(|x| x.0).unwrap_or("")
}

/// Collections a tree change writes.
pub const TREE_WRITE: &[&str] = &[
    "project_versions",
    "context_versions",
    "container_versions",
    "component_versions",
    "code_versions",
    "test_versions",
    "has_version",
    "contains",
    "based_on",
];

/// One project version's tree, resolved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TreeIndex {
    /// `project_versions/<key>`.
    pub root: String,
    pub rev: i64,
    /// Node id → version id, for every node in the tree.
    pub version_of: BTreeMap<String, String>,
    /// Version id → node id.
    pub node_of: BTreeMap<String, String>,
    /// Version id → child version ids (sorted).
    pub children: BTreeMap<String, Vec<String>>,
    /// Node id → parent node id within this tree (contexts have the project node).
    pub parent: BTreeMap<String, String>,
    /// Version id → content hash.
    pub content_hash: BTreeMap<String, String>,
}

impl TreeIndex {
    /// The version of `node` in this tree.
    pub fn version(&self, node: &str) -> Option<&str> {
        self.version_of.get(node).map(String::as_str)
    }
    /// Nodes of one collection present in this tree.
    pub fn nodes_in<'a>(&'a self, coll: &'a str) -> impl Iterator<Item = &'a String> + 'a {
        self.version_of.keys().filter(move |n| coll_of(n) == coll)
    }
    /// Child nodes of a node in this tree.
    pub fn child_nodes(&self, node: &str) -> Vec<String> {
        self.version(node)
            .and_then(|v| self.children.get(v))
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| self.node_of.get(c).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }
    /// Every node beneath `node` (not including it).
    pub fn descendants(&self, node: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut todo = self.child_nodes(node);
        while let Some(n) = todo.pop() {
            todo.extend(self.child_nodes(&n));
            out.push(n);
        }
        out
    }
    /// The node and its ancestors in this tree, nearest first (ends at the project node).
    pub fn ancestors(&self, node: &str) -> Vec<String> {
        let mut out = vec![node.to_string()];
        let mut cur = node.to_string();
        while let Some(p) = self.parent.get(&cur) {
            if out.contains(p) {
                break;
            }
            out.push(p.clone());
            cur = p.clone();
        }
        out
    }
}

/// Changes to apply to an open project version's tree.
#[derive(Debug, Clone, Default)]
pub struct TreeChanges {
    /// Leaf node (`code/…`, `tests/…`) → its new version id, or `None` to remove it.
    pub leaves: BTreeMap<String, Option<String>>,
    /// C4 node → its new definition (creates a new version even with unchanged children).
    pub defs: BTreeMap<String, Value>,
    /// C4 nodes to remove (with everything beneath them).
    pub removed: BTreeSet<String>,
    /// Work item the change belongs to (recorded on new `has_version` edges).
    pub work_item: Option<String>,
}

impl TreeChanges {
    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty() && self.defs.is_empty() && self.removed.is_empty()
    }
}

/// Key of a version of `node` with the given hash (`tree_hash` for C4 levels, `content_hash`
/// for leaves): unchanged content under the same node always gets the same key, so identical
/// subtrees are shared between project versions.
pub fn version_key(project: &str, node: &str, hash: &str) -> String {
    qualify(
        project,
        &blake3::hash(format!("{node}|{hash}").as_bytes()).to_hex()[..32],
    )
}

/// Tree hash of a C4 version: its definition's content hash and its sorted children.
pub fn tree_hash(content_hash: &str, children: &[String]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"kodehub-tree-v1\n");
    h.update(content_hash.as_bytes());
    for c in children {
        h.update(b"\n");
        h.update(c.as_bytes());
    }
    h.finalize().to_hex().to_string()
}

impl Store {
    // ------------------------------------------------------------------ project versions

    /// The project's registry document.
    async fn project_registry(&self, project: &str) -> Result<Value> {
        self.g_get(crate::schema::PROJECTS, project)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("project {project}")))
    }

    /// Id of the active project's current (default) open project version, creating `r1` when
    /// the project has none yet.
    pub async fn current_version(&self) -> Result<String> {
        let p = self.db.require_project()?;
        if let Some(cv) = self.project_registry(&p).await?["current_version"].as_str() {
            return Ok(cv.to_string());
        }
        let pv = self.create_project_version(None, None, "kodehub").await?;
        let id = pv["_id"].as_str().unwrap_or_default().to_string();
        self.set_current_version(&id).await?;
        Ok(id)
    }

    /// Make an open project version the project's default.
    pub async fn set_current_version(&self, pv: &str) -> Result<()> {
        let pv = self.pv_id(pv)?;
        let doc = self.get_node(&pv).await?;
        if doc["state"] != "open" {
            return Err(StoreError::Invalid(format!(
                "{pv} is frozen; only an open project version can be current"
            )));
        }
        let p = self.db.require_project()?;
        self.g_patch(crate::schema::PROJECTS, &p, &json!({"current_version": pv}))
            .await?;
        Ok(())
    }

    /// `project_versions/<key>` for a key, id or name (`r3`).
    pub fn pv_id(&self, pv: &str) -> Result<String> {
        if pv.starts_with("project_versions/") {
            return self.db.qid(pv);
        }
        Ok(format!("project_versions/{}", self.db.pkey(pv)?))
    }

    async fn next_version_number(&self) -> Result<i64> {
        Ok(self
            .aql(
                "RETURN MAX(FOR v IN @@project_versions RETURN v.number) || 0",
                json!({}),
            )
            .await?
            .first()
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1)
    }

    /// Create an open project version. With `based_on`, it starts with that version's tree
    /// (the next release, or a hotfix of a frozen one).
    pub async fn create_project_version(
        &self,
        name: Option<&str>,
        based_on: Option<&str>,
        by: &str,
    ) -> Result<Value> {
        let p = self.db.require_project()?;
        let n = self.next_version_number().await?;
        let key = qualify(&p, &format!("r{n}"));
        let id = format!("project_versions/{key}");
        let base = match based_on {
            Some(b) => Some(self.pv_id(b)?),
            None => None,
        };
        let trx = self.db.begin(TREE_WRITE, &[]).await?;
        let res: Result<Value> = async {
            let doc = self
                .db
                .insert(
                    "project_versions",
                    &json!({
                        "_key": key, "name": name.map(str::to_string).filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| format!("r{n}")),
                        "number": n, "state": "open", "based_on": base, "tree_rev": 0,
                        "created_at": now_iso(), "created_by": by,
                    }),
                    Some(&trx),
                )
                .await?;
            let pid = format!("{}/{p}", crate::schema::PROJECTS);
            self.db
                .insert(
                    "has_version",
                    &json!({"_key": edge_key("has_version", &pid, &id), "_from": pid, "_to": id,
                            "state": "accepted", "created_at": now_iso()}),
                    Some(&trx),
                )
                .await?;
            if let Some(b) = &base {
                self.db
                    .insert(
                        "based_on",
                        &json!({"_key": edge_key("based_on", &id, b), "_from": id, "_to": b,
                                "created_at": now_iso()}),
                        Some(&trx),
                    )
                    .await?;
                let roots = self
                    .db
                    .aql(
                        "FOR e IN @@contains FILTER e._from == @b RETURN e._to",
                        json!({"b": b}),
                        Some(&trx),
                    )
                    .await?;
                for r in roots.iter().filter_map(Value::as_str) {
                    self.db
                        .insert(
                            "contains",
                            &json!({"_key": edge_key("contains", &id, r), "_from": id, "_to": r}),
                            Some(&trx),
                        )
                        .await?;
                }
            }
            Ok(doc)
        }
        .await;
        match res {
            Ok(d) => {
                self.db.commit(&trx).await?;
                Ok(d)
            }
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                Err(e)
            }
        }
    }

    /// Freeze an open project version: it never changes again (it may still be deployed).
    /// When it was the project's current version, the next one opens (`based_on` it, with the
    /// same tree) and becomes current, so work carries on.
    pub async fn freeze_project_version(&self, pv: &str, by: &str, reason: &str) -> Result<Value> {
        let pv = self.pv_id(pv)?;
        let doc = self.get_node(&pv).await?;
        if doc["state"] == "frozen" {
            return Ok(doc);
        }
        let frozen = self
            .db
            .patch(
                "project_versions",
                crate::key_of(&pv),
                &json!({"state": "frozen", "frozen_at": now_iso(), "frozen_by": by,
                        "frozen_reason": reason}),
                None,
            )
            .await?;
        if self.current_version().await? == pv {
            let next = self.create_project_version(None, Some(&pv), by).await?;
            self.set_current_version(next["_id"].as_str().unwrap_or_default())
                .await?;
        }
        Ok(frozen)
    }

    /// Every project version, newest first.
    pub async fn list_project_versions(&self) -> Result<Vec<Value>> {
        self.aql(
            "FOR v IN @@project_versions SORT v.number DESC RETURN v",
            json!({}),
        )
        .await
    }

    // ------------------------------------------------------------------ resolving a tree

    /// The tree of a project version (`None` = the current one), cached per revision.
    pub async fn tree(&self, pv: Option<&str>) -> Result<Arc<TreeIndex>> {
        let id = match pv {
            Some(v) => self.pv_id(v)?,
            None => self.current_version().await?,
        };
        let doc = self.get_node(&id).await?;
        let rev = doc["tree_rev"].as_i64().unwrap_or(0);
        if let Some((r, t)) = self.trees.lock().unwrap().get(&id) {
            if *r == rev {
                return Ok(t.clone());
            }
        }
        let t = Arc::new(self.load_tree(&id, rev, None).await?);
        self.trees.lock().unwrap().insert(id, (rev, t.clone()));
        Ok(t)
    }

    /// Walk a project version's tree (inside `trx` when given).
    pub async fn load_tree(&self, pv_id: &str, rev: i64, trx: Option<&str>) -> Result<TreeIndex> {
        let rows = self
            .db
            .aql(
                "FOR v, e IN 1..8 OUTBOUND @pv @@contains
                   RETURN {id: v._id, node: v.node, parent: e._from, hash: v.content_hash}",
                json!({"pv": pv_id}),
                trx,
            )
            .await?;
        let project = self.db.require_project()?;
        let project_node = format!("{}/{project}", crate::schema::PROJECTS);
        let mut t = TreeIndex {
            root: pv_id.to_string(),
            rev,
            ..Default::default()
        };
        for r in &rows {
            let (Some(id), Some(node)) = (r["id"].as_str(), r["node"].as_str()) else {
                continue;
            };
            t.version_of.insert(node.to_string(), id.to_string());
            t.node_of.insert(id.to_string(), node.to_string());
            if let Some(h) = r["hash"].as_str() {
                t.content_hash.insert(id.to_string(), h.to_string());
            }
            if let Some(parent) = r["parent"].as_str() {
                t.children
                    .entry(parent.to_string())
                    .or_default()
                    .push(id.to_string());
            }
        }
        for cs in t.children.values_mut() {
            cs.sort();
            cs.dedup();
        }
        for r in &rows {
            let (Some(node), Some(parent)) = (r["node"].as_str(), r["parent"].as_str()) else {
                continue;
            };
            let pnode = if parent == pv_id {
                project_node.clone()
            } else {
                t.node_of.get(parent).cloned().unwrap_or_default()
            };
            t.parent.insert(node.to_string(), pnode);
        }
        Ok(t)
    }

    /// The version of `node` in project version `pv` (`None` = current).
    pub async fn version_in(&self, node: &str, pv: Option<&str>) -> Result<Option<String>> {
        let node = self.db.qid(node)?;
        Ok(self.tree(pv).await?.version(&node).map(str::to_string))
    }

    // ------------------------------------------------------------------ changing a tree

    /// The identity parent of a node (`contains` between unversioned nodes).
    async fn identity_parent(&self, node: &str, trx: Option<&str>) -> Result<Option<String>> {
        Ok(self
            .db
            .aql(
                "FOR e IN @@contains FILTER e._to == @n
                   AND PARSE_IDENTIFIER(e._from).collection IN ['projects', 'contexts', 'containers', 'components']
                   RETURN e._from",
                json!({"n": node}),
                trx,
            )
            .await?
            .into_iter()
            .find_map(|v| v.as_str().map(str::to_string)))
    }

    /// Apply changes to an open project version's tree in one transaction: new versions for
    /// every affected ancestor (each listing all its children), then the root moves. Fails with
    /// `Conflict` when the project version changed since `expect_rev` (when given).
    pub async fn apply_tree_changes(
        &self,
        pv: Option<&str>,
        changes: &TreeChanges,
        by: &str,
        expect_rev: Option<i64>,
    ) -> Result<Arc<TreeIndex>> {
        let pv_id = match pv {
            Some(v) => self.pv_id(v)?,
            None => self.current_version().await?,
        };
        if changes.is_empty() {
            return self.tree(Some(&pv_id)).await;
        }
        let trx = self.db.begin(TREE_WRITE, &[]).await?;
        let res = self
            .apply_tree_changes_in(&pv_id, changes, by, expect_rev, &trx)
            .await;
        match res {
            Ok(rev) => {
                self.db.commit(&trx).await?;
                let t = Arc::new(self.load_tree(&pv_id, rev, None).await?);
                self.trees
                    .lock()
                    .unwrap()
                    .insert(pv_id.clone(), (rev, t.clone()));
                Ok(t)
            }
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                Err(e)
            }
        }
    }

    /// `apply_tree_changes` inside a caller's transaction (which must include `TREE_WRITE`).
    /// Returns the new tree revision.
    pub async fn apply_tree_changes_in(
        &self,
        pv_id: &str,
        changes: &TreeChanges,
        by: &str,
        expect_rev: Option<i64>,
        trx: &str,
    ) -> Result<i64> {
        let project = self.db.require_project()?;
        let project_node = format!("{}/{project}", crate::schema::PROJECTS);
        let pv_key = crate::key_of(pv_id).to_string();
        let pv_doc = self.db.get("project_versions", &pv_key, Some(trx)).await?;
        if pv_doc["state"] != "open" {
            return Err(StoreError::Immutable(format!(
                "{pv_id} is frozen; changes go into an open project version"
            )));
        }
        let rev = pv_doc["tree_rev"].as_i64().unwrap_or(0);
        if expect_rev.is_some_and(|r| r != rev) {
            return Err(StoreError::Conflict(format!(
                "{pv_id} moved (revision {rev}); rebuild the change on the current tree"
            )));
        }
        let tree = self.load_tree(pv_id, rev, Some(trx)).await?;

        // where each changed node lives now (identity hierarchy)
        let mut parent_of: BTreeMap<String, Option<String>> = BTreeMap::new();
        let changed: BTreeSet<String> = changes
            .leaves
            .keys()
            .chain(changes.defs.keys())
            .chain(changes.removed.iter())
            .cloned()
            .collect();
        for n in &changed {
            let p = self.identity_parent(n, Some(trx)).await?;
            parent_of.insert(n.clone(), p);
        }
        // affected C4 ancestors: walk up the identity hierarchy from every changed node, and
        // from the old parent of nodes that moved or were removed
        let mut affected: BTreeSet<String> = BTreeSet::new();
        let mut frontier: Vec<String> = Vec::new();
        for n in &changed {
            if let Some(Some(p)) = parent_of.get(n) {
                frontier.push(p.clone());
            }
            if let Some(old) = tree.parent.get(n) {
                frontier.push(old.clone());
            }
            if C4_LEVELS.contains(&coll_of(n)) && !changes.removed.contains(n) {
                affected.insert(n.clone());
            }
        }
        while let Some(n) = frontier.pop() {
            if n == project_node || n == pv_id || !C4_LEVELS.contains(&coll_of(&n)) {
                continue;
            }
            if affected.insert(n.clone()) {
                let p = match parent_of.get(&n) {
                    Some(p) => p.clone(),
                    None => {
                        let p = self.identity_parent(&n, Some(trx)).await?;
                        parent_of.insert(n.clone(), p.clone());
                        p
                    }
                };
                if let Some(p) = p {
                    frontier.push(p);
                }
                if let Some(old) = tree.parent.get(&n) {
                    frontier.push(old.clone());
                }
            }
        }
        // nodes removed, including everything beneath a removed C4 node
        let mut gone: BTreeSet<String> = changes
            .leaves
            .iter()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.clone())
            .collect();
        for r in &changes.removed {
            gone.insert(r.clone());
            gone.extend(tree.descendants(r));
            affected.remove(r);
        }
        // the new version of every node, starting from the current tree
        let mut new_version: BTreeMap<String, String> = tree.version_of.clone();
        for (n, v) in &changes.leaves {
            match v {
                Some(v) => {
                    new_version.insert(n.clone(), v.clone());
                }
                None => {
                    new_version.remove(n);
                }
            }
        }
        for g in &gone {
            new_version.remove(g);
        }
        // the effective parent of every node after the change
        let mut eff_parent: BTreeMap<String, String> = tree.parent.clone();
        for (n, p) in &parent_of {
            match p {
                Some(p) => {
                    eff_parent.insert(n.clone(), p.clone());
                }
                None => {
                    eff_parent.remove(n);
                }
            }
        }
        // rebuild affected C4 versions bottom-up: components, containers, contexts
        let mut ordered: Vec<String> = affected.into_iter().collect();
        ordered.sort_by_key(|n| std::cmp::Reverse(level_rank(coll_of(n))));
        for n in &ordered {
            let coll = coll_of(n).to_string();
            let vcoll = version_collection(&coll).expect("C4 level");
            let def = match changes.defs.get(n) {
                Some(d) => d.clone(),
                None => match tree.version(n) {
                    Some(v) => {
                        self.db.get(vcoll, crate::key_of(v), Some(trx)).await?["definition"].clone()
                    }
                    None => {
                        let doc = self.db.get(&coll, crate::key_of(n), Some(trx)).await?;
                        crate::c4::c4_definition(&coll, &doc)
                    }
                },
            };
            let mut kids: Vec<String> = eff_parent
                .iter()
                .filter(|(c, p)| *p == n && !gone.contains(*c))
                .filter_map(|(c, _)| new_version.get(c).cloned())
                .collect();
            kids.sort();
            kids.dedup();
            let chash = crate::c4::c4_hash(&def);
            let th = tree_hash(&chash, &kids);
            let vkey = version_key(&project, n, &th);
            let vid = format!("{vcoll}/{vkey}");
            if tree.version(n) == Some(vid.as_str()) {
                continue;
            }
            self.db
                .insert_ignore(
                    vcoll,
                    &json!({"_key": vkey, "node": n, "definition": def, "content_hash": chash,
                            "tree_hash": th, "children": kids, "created_at": now_iso(),
                            "created_by": by}),
                    Some(trx),
                )
                .await?;
            let seq = self.next_seq(n, Some(trx)).await?;
            self.db
                .insert_ignore(
                    "has_version",
                    &json!({"_key": edge_key("has_version", n, &vid), "_from": n, "_to": vid,
                            "seq": seq, "state": "accepted", "work_item": changes.work_item,
                            "created_at": now_iso(), "created_by": by}),
                    Some(trx),
                )
                .await?;
            for k in &kids {
                self.db
                    .insert_ignore(
                        "contains",
                        &json!({"_key": edge_key("contains", &vid, k), "_from": vid, "_to": k}),
                        Some(trx),
                    )
                    .await?;
            }
            new_version.insert(n.clone(), vid);
        }
        // leaves accepted into the tree: their has_version edges become accepted
        for (n, v) in &changes.leaves {
            if let Some(v) = v {
                self.db
                    .aql(
                        "FOR e IN @@has_version FILTER e._from == @n AND e._to == @v
                           UPDATE e WITH {state: 'accepted', accepted_at: @now, accepted_into: @pv} IN @@has_version",
                        json!({"n": n, "v": v, "now": now_iso(), "pv": pv_id}),
                        Some(trx),
                    )
                    .await?;
            }
        }
        // the root: one edge per context, pointing at its new version
        let roots: BTreeSet<String> = eff_parent
            .iter()
            .filter(|(c, p)| **p == project_node && !gone.contains(*c))
            .filter_map(|(c, _)| new_version.get(c).cloned())
            .collect();
        let current: BTreeSet<String> = tree
            .children
            .get(pv_id)
            .map(|v| v.iter().cloned().collect())
            .unwrap_or_default();
        for old in current.difference(&roots) {
            self.db
                .aql(
                    "FOR e IN @@contains FILTER e._from == @pv AND e._to == @v REMOVE e IN @@contains",
                    json!({"pv": pv_id, "v": old}),
                    Some(trx),
                )
                .await?;
        }
        for new in roots.difference(&current) {
            self.db
                .insert(
                    "contains",
                    &json!({"_key": edge_key("contains", pv_id, new), "_from": pv_id, "_to": new}),
                    Some(trx),
                )
                .await?;
        }
        // compare-and-set: only one writer moves a root at a time
        let updated = self
            .db
            .aql(
                "FOR d IN @@project_versions FILTER d._key == @k AND d.tree_rev == @rev
                   UPDATE d WITH {tree_rev: @rev + 1, updated_at: @now, updated_by: @by} IN @@project_versions
                   RETURN NEW.tree_rev",
                json!({"k": pv_key, "rev": rev, "now": now_iso(), "by": by}),
                Some(trx),
            )
            .await?;
        if updated.is_empty() {
            return Err(StoreError::Conflict(format!(
                "{pv_id} moved during the change; retry"
            )));
        }
        Ok(rev + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_keys_are_content_addressed() {
        let a = version_key("p", "code/p.x", "h1");
        assert_eq!(a, version_key("p", "code/p.x", "h1"));
        assert_ne!(a, version_key("p", "code/p.y", "h1"));
        assert_ne!(a, version_key("p", "code/p.x", "h2"));
        assert!(a.starts_with("p."));
    }

    #[test]
    fn tree_hash_depends_on_children() {
        let a = tree_hash("c", &["x".into(), "y".into()]);
        assert_eq!(a, tree_hash("c", &["x".into(), "y".into()]));
        assert_ne!(a, tree_hash("c", &["x".into()]));
        assert_ne!(a, tree_hash("d", &["x".into(), "y".into()]));
    }

    #[test]
    fn index_walks() {
        let mut t = TreeIndex::default();
        for (n, v) in [
            ("contexts/p.a", "context_versions/p.1"),
            ("components/p.b", "component_versions/p.2"),
            ("code/p.c", "code_versions/p.3"),
        ] {
            t.version_of.insert(n.into(), v.into());
            t.node_of.insert(v.into(), n.into());
        }
        t.children.insert(
            "context_versions/p.1".into(),
            vec!["component_versions/p.2".into()],
        );
        t.children.insert(
            "component_versions/p.2".into(),
            vec!["code_versions/p.3".into()],
        );
        t.parent.insert("code/p.c".into(), "components/p.b".into());
        t.parent
            .insert("components/p.b".into(), "contexts/p.a".into());
        t.parent.insert("contexts/p.a".into(), "projects/p".into());
        assert_eq!(
            t.descendants("contexts/p.a"),
            vec!["components/p.b".to_string(), "code/p.c".to_string()]
        );
        assert_eq!(
            t.ancestors("code/p.c"),
            vec!["code/p.c", "components/p.b", "contexts/p.a", "projects/p"]
        );
    }
}
