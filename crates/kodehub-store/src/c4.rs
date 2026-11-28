//! The C4 layer (spec §5.1–5.2, §7, §14.4, §15; docs/ontology.md): the philosophy document,
//! C4 node definitions (versioned through the tree, `tree.rs`), requirement kinds and
//! inheritance, per-node requirement and test counts, use cases, and node lifecycle states with
//! roll-up.

use std::collections::{BTreeMap, BTreeSet};

use kodehub_core::model::{now_iso, Kind};
use serde_json::{json, Value};

use crate::store::{edge_key, key_of};
use crate::{Result, Store, StoreError};

/// C4 levels above code, top first.
pub const C4_COLLECTIONS: &[&str] = &["contexts", "containers", "components"];

/// Local key of the singular philosophy node of a project (its id is
/// `philosophy/<project>.philosophy`, see `Store::philosophy_id`).
pub const PHILOSOPHY_KEY: &str = "philosophy";

/// The philosophy node's id in `project`.
pub fn philosophy_id_of(project: &str) -> String {
    format!(
        "philosophy/{}",
        crate::arango::qualify(project, PHILOSOPHY_KEY)
    )
}

/// The project's own node (`projects/<id>`): the root of its C4 hierarchy and the target of
/// project-level work.
pub fn project_node_of(project: &str) -> String {
    format!("{}/{project}", crate::schema::PROJECTS)
}

/// Requirement kinds known today; the field is open so the list can grow (spec §7.1). A
/// `decision` is a recorded choice with its rationale (D-062 / ontology mapping).
pub const REQUIREMENT_KINDS: &[&str] = &["business", "technical", "decision"];

/// Lifecycle states in order (spec §14.4).
pub const LIFECYCLE: &[&str] = &["planned", "in_build", "ready_for_test", "live"];

fn rank(s: &str) -> usize {
    LIFECYCLE.iter().position(|x| *x == s).unwrap_or(0)
}

pub fn level_of(id: &str) -> &'static str {
    match id.split_once('/').map(|x| x.0) {
        Some("projects") | Some("philosophy") => "project",
        Some("contexts") => "context",
        Some("containers") => "container",
        Some("components") => "component",
        Some("code") => "code",
        Some("tests") => "test",
        _ => "other",
    }
}

/// The hashed definition of a C4 node: everything except identity and bookkeeping.
pub fn c4_definition(collection: &str, doc: &Value) -> Value {
    let mut def = serde_json::Map::new();
    def.insert("level".into(), json!(collection));
    for k in [
        "title",
        "name",
        "description",
        "technology",
        "tags",
        "attributes",
        "interface",
        "container_spec",
    ] {
        if let Some(v) = doc.get(k) {
            if !v.is_null() {
                def.insert(k.into(), v.clone());
            }
        }
    }
    Value::Object(def)
}

pub fn c4_hash(def: &Value) -> String {
    kodehub_core::canonical::hash_value(def)
}

/// Roll up lifecycle states: all planned → planned; any partly built → in_build; otherwise the
/// least advanced state.
pub fn roll_up(states: &[&str]) -> &'static str {
    if states.is_empty() {
        return "planned";
    }
    if states.iter().all(|s| *s == "planned") {
        return "planned";
    }
    let min = states.iter().map(|s| rank(s)).min().unwrap_or(0);
    if min == 0 {
        "in_build"
    } else {
        LIFECYCLE[min]
    }
}

impl Store {
    /// The active project's node (`projects/<id>`).
    pub fn project_node(&self) -> Result<String> {
        Ok(project_node_of(&self.db.require_project()?))
    }

    /// Record that the project owns a top-level node (`owns`, for traversal from the project).
    pub async fn own(&self, id: &str) -> Result<()> {
        let p = self.project_node()?;
        self.db
            .insert_ignore(
                "owns",
                &json!({"_key": edge_key("owns", &p, id), "_from": p, "_to": id,
                        "created_at": now_iso()}),
                None,
            )
            .await
    }

    // ------------------------------------------------------------------ philosophy

    /// The philosophy node's id in the active project.
    pub fn philosophy_id(&self) -> Result<String> {
        self.db.pid("philosophy", PHILOSOPHY_KEY)
    }

    pub async fn get_philosophy(&self) -> Result<Option<Value>> {
        match self
            .db
            .get("philosophy", &self.db.pkey(PHILOSOPHY_KEY)?, None)
            .await
        {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Set the project's philosophy document (spec §7.1). Keeps prior text in `history`.
    pub async fn set_philosophy(&self, text: &str, by: &str) -> Result<Value> {
        self.set_philosophy_with(text, by, None, None).await
    }

    pub async fn set_philosophy_with(
        &self,
        text: &str,
        by: &str,
        title: Option<&str>,
        raw_transcript: Option<&str>,
    ) -> Result<Value> {
        let now = now_iso();
        match self.get_philosophy().await? {
            Some(cur) => {
                if cur["text"].as_str() == Some(text.trim()) && raw_transcript.is_none() {
                    return Ok(cur);
                }
                let mut hist = cur["history"].as_array().cloned().unwrap_or_default();
                hist.push(json!({"revision": cur["revision"], "text": cur["text"],
                                 "updated_at": cur["updated_at"], "updated_by": cur["updated_by"]}));
                let mut patch = json!({
                    "text": text.trim(), "revision": cur["revision"].as_i64().unwrap_or(1) + 1,
                    "updated_at": now, "updated_by": by, "history": hist,
                });
                if let Some(t) = title {
                    patch["title"] = json!(t);
                }
                if let Some(r) = raw_transcript {
                    patch["raw_transcript"] = json!(r);
                }
                self.db
                    .patch("philosophy", &self.db.pkey(PHILOSOPHY_KEY)?, &patch, None)
                    .await
            }
            None => {
                self.db
                    .insert(
                        "philosophy",
                        &json!({"_key": self.db.pkey(PHILOSOPHY_KEY)?, "title": title.unwrap_or("Philosophy"),
                                "text": text.trim(), "raw_transcript": raw_transcript,
                                "revision": 1, "history": [], "created_at": now,
                                "updated_at": now, "updated_by": by}),
                        None,
                    )
                    .await?;
                self.own(&self.philosophy_id()?).await?;
                self.db
                    .get("philosophy", &self.db.pkey(PHILOSOPHY_KEY)?, None)
                    .await
            }
        }
    }

    // ------------------------------------------------------------------ C4 definitions

    /// Give a C4 node a version matching its current definition in the current project
    /// version (the node must be placed: a `contains` edge from its parent). Returns the
    /// version id, or `None` while the node is not placed.
    pub async fn sync_c4_version(
        &self,
        collection: &str,
        key: &str,
        by: &str,
    ) -> Result<Option<String>> {
        if !C4_COLLECTIONS.contains(&collection) {
            return Err(StoreError::Invalid(format!(
                "{collection} is not a C4 level"
            )));
        }
        let key = &self.db.pkey(key)?;
        let doc = self.db.get(collection, key, None).await?;
        let node_id = format!("{collection}/{key}");
        let placed = !self
            .aql(
                "FOR e IN @@contains FILTER e._to == @n LIMIT 1 RETURN 1",
                json!({"n": node_id}),
            )
            .await?
            .is_empty();
        if !placed {
            return Ok(None);
        }
        let def = c4_definition(collection, &doc);
        let tree = self.tree(None).await?;
        let hash = c4_hash(&def);
        if let Some(v) = tree.version(&node_id) {
            if tree.content_hash.get(v) == Some(&hash) {
                return Ok(Some(v.to_string()));
            }
        }
        let mut ch = crate::tree::TreeChanges::default();
        ch.defs.insert(node_id.clone(), def);
        let t = self.apply_tree_changes(None, &ch, by, None).await?;
        Ok(t.version(&node_id).map(str::to_string))
    }

    /// Place every node and refresh every definition in the current project version (after a
    /// seed or an import).
    pub async fn sync_all_c4_versions(&self, by: &str) -> Result<usize> {
        self.sync_tree(by).await
    }

    // ------------------------------------------------------------------ requirements

    /// The node itself and its ancestors up to the project node, nearest first (identity
    /// hierarchy, so planned nodes have ancestors too; a method's class comes first).
    pub async fn ancestors(&self, node_id: &str) -> Result<Vec<String>> {
        let node_id = &self.db.qid(node_id)?;
        let mut chain = vec![node_id.to_string()];
        let mut cur = node_id.to_string();
        let project = self.project_node()?;
        for _ in 0..12 {
            let parent = self
                .aql(
                    "LET c = FIRST(FOR e IN @@member_of FILTER e._from == @n RETURN e._to)
                     LET p = FIRST(FOR e IN @@contains FILTER e._to == @n
                                     AND PARSE_IDENTIFIER(e._from).collection IN ['projects', 'contexts', 'containers', 'components']
                                     RETURN e._from)
                     RETURN c || p",
                    json!({"n": cur}),
                )
                .await?
                .into_iter()
                .next()
                .and_then(|v| v.as_str().map(str::to_string));
            match parent {
                Some(p) if !chain.contains(&p) => {
                    chain.push(p.clone());
                    cur = p;
                }
                _ => break,
            }
        }
        if !chain.contains(&project) {
            chain.push(project);
        }
        Ok(chain)
    }

    /// Every requirement that applies to a node: its own plus everything inherited from its
    /// ancestors up to the project (spec §7.2), nearest first.
    pub async fn requirements_for(&self, node_id: &str) -> Result<Value> {
        let node_id = &self.db.qid(node_id)?;
        let chain = self.ancestors(node_id).await?;
        let mut seen = BTreeSet::new();
        let mut rows = Vec::new();
        for (distance, n) in chain.iter().enumerate() {
            let direct = self
                .aql(
                    "FOR e IN @@applies_to FILTER e._to == @n LET r = DOCUMENT(e._from)
                       FILTER r != null SORT r.title RETURN r",
                    json!({"n": n}),
                )
                .await?;
            for r in direct {
                let id = r["_id"].as_str().unwrap_or_default().to_string();
                if !seen.insert(id.clone()) {
                    continue;
                }
                rows.push(json!({
                    "id": id, "key": r["_key"], "title": r["title"], "statement": r["statement"],
                    "kind": r["kind"].as_str().unwrap_or("technical"), "tags": r["tags"],
                    "attached_to": n, "attached_level": level_of(n),
                    "inherited": distance > 0, "distance": distance,
                }));
            }
        }
        Ok(json!({
            "node": node_id, "chain": chain, "requirements": rows,
            "philosophy": self.get_philosophy().await?.map(|p| p["text"].clone()),
        }))
    }

    /// Identity children of every node (C4 `contains`, and methods under their class).
    async fn identity_children(&self) -> Result<BTreeMap<String, Vec<String>>> {
        let kids = self
            .aql(
                "FOR e IN UNION(
                    (FOR x IN @@contains
                       FILTER PARSE_IDENTIFIER(x._from).collection IN ['projects', 'contexts', 'containers', 'components']
                       RETURN {p: x._from, c: x._to}),
                    (FOR x IN @@member_of RETURN {p: x._to, c: x._from}))
                   RETURN e",
                json!({}),
            )
            .await?;
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for k in &kids {
            if let (Some(p), Some(c)) = (k["p"].as_str(), k["c"].as_str()) {
                children.entry(p.into()).or_default().push(c.into());
            }
        }
        Ok(children)
    }

    /// Requirement counts per node (spec §8.3): own and own-plus-children, by kind.
    pub async fn requirement_counts(&self) -> Result<BTreeMap<String, Value>> {
        let applies = self
            .aql(
                "FOR e IN @@applies_to LET r = DOCUMENT(e._from) FILTER r != null
                   RETURN {n: e._to, r: r._id, kind: r.kind || 'technical'}",
                json!({}),
            )
            .await?;
        let children = self.identity_children().await?;
        let mut own: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
        for s in &applies {
            if let (Some(n), Some(r)) = (s["n"].as_str(), s["r"].as_str()) {
                own.entry(n.into())
                    .or_default()
                    .insert((r.into(), s["kind"].as_str().unwrap_or("technical").into()));
            }
        }
        fn subtree(
            n: &str,
            children: &BTreeMap<String, Vec<String>>,
            own: &BTreeMap<String, BTreeSet<(String, String)>>,
            depth: usize,
        ) -> BTreeSet<(String, String)> {
            let mut out = own.get(n).cloned().unwrap_or_default();
            if depth < 12 {
                for c in children.get(n).map(Vec::as_slice).unwrap_or_default() {
                    out.extend(subtree(c, children, own, depth + 1));
                }
            }
            out
        }
        let count = |set: &BTreeSet<(String, String)>| {
            let mut m = serde_json::Map::new();
            for k in ["business", "technical"] {
                m.insert(k.into(), json!(0));
            }
            for (_, kind) in set {
                if kind == "decision" {
                    continue;
                }
                let e = m.entry(kind.clone()).or_insert(json!(0));
                *e = json!(e.as_u64().unwrap_or(0) + 1);
            }
            Value::Object(m)
        };
        let mut nodes: BTreeSet<String> = own.keys().cloned().collect();
        nodes.extend(children.keys().cloned());
        let mut out = BTreeMap::new();
        for n in nodes {
            let o = own.get(&n).cloned().unwrap_or_default();
            let t = subtree(&n, &children, &own, 0);
            out.insert(n, json!({"own": count(&o), "total": count(&t)}));
        }
        Ok(out)
    }

    /// Test counts per node (spec §8.3, §14.4) in the current project version: `own` (tests
    /// reaching a code node, or tests a C4 node directly holds) and `total` (everything
    /// beneath), each with passed / failed / total from the latest formal run at the current
    /// closure, plus how many of the tests are built.
    pub async fn test_counts(&self) -> Result<BTreeMap<String, Value>> {
        let v = self.view(None).await?;
        let tally = |set: &BTreeSet<String>| {
            let (mut p, mut f, mut b) = (0u64, 0u64, 0u64);
            let mut last: Option<String> = None;
            for t in set {
                if let Some(r) = v.states.get(t) {
                    match r.status.as_str() {
                        "pass" => p += 1,
                        "fail" => f += 1,
                        _ => {}
                    }
                    if r.built {
                        b += 1;
                    }
                    if let Some(at) = &r.ran_at {
                        if last.as_deref().is_none_or(|l| at.as_str() > l) {
                            last = Some(at.clone());
                        }
                    }
                }
            }
            json!({"passed": p, "failed": f, "total": set.len(), "built": b, "last_run": last})
        };
        let mut per_leaf: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut out = BTreeMap::new();
        for s in v.g.symbols.values() {
            let set: BTreeSet<String> = if s.kind == Kind::Test {
                [s.id.clone()].into()
            } else {
                v.g.tests_reaching(&s.id).into_iter().collect()
            };
            let id = crate::queries::node_id(&v.g, &s.id);
            out.insert(
                id.clone(),
                json!({"own": tally(&set), "total": tally(&set)}),
            );
            per_leaf.insert(id, set);
        }
        for n in v
            .tree
            .version_of
            .keys()
            .filter(|n| !per_leaf.contains_key(*n))
        {
            let own: BTreeSet<String> = v
                .tree
                .child_nodes(n)
                .iter()
                .filter(|c| c.starts_with("tests/"))
                .map(|c| key_of(c).to_string())
                .collect();
            let mut total: BTreeSet<String> = BTreeSet::new();
            for d in v.tree.descendants(n) {
                if let Some(s) = per_leaf.get(&d) {
                    total.extend(s.iter().cloned());
                }
            }
            out.insert(
                n.clone(),
                json!({"own": tally(&own), "total": tally(&total)}),
            );
        }
        Ok(out)
    }

    /// The tests covering a node (a code node: the tests reaching it; a C4 node: every test
    /// reaching code beneath it) with their latest formal result and recent history.
    pub async fn tests_for_node(&self, node: &str) -> Result<Vec<Value>> {
        let node = &self.db.qid(node)?;
        let v = self.view(None).await?;
        let mut tests: BTreeSet<String> = BTreeSet::new();
        let mut leaves: Vec<String> = Vec::new();
        if node.starts_with("code/") || node.starts_with("tests/") {
            leaves.push(key_of(node).to_string());
        } else {
            for d in v.tree.descendants(node) {
                if d.starts_with("code/") || d.starts_with("tests/") {
                    leaves.push(key_of(&d).to_string());
                }
            }
        }
        for k in &leaves {
            match v.g.symbols.get(k) {
                Some(x) if x.kind == Kind::Test => {
                    tests.insert(k.clone());
                }
                Some(_) => tests.extend(v.g.tests_reaching(k)),
                None => {}
            }
        }
        let mut out = Vec::new();
        for t in &tests {
            let Some(s) = v.g.symbols.get(t) else {
                continue;
            };
            let st = v.states.get(t);
            let runs = self
                .aql(
                    "FOR r IN @@test_logs FILTER r.test == @t AND r.kind != 'run'
                       SORT r.ran_at DESC LIMIT 6
                       RETURN {status: r.status, ran_at: r.ran_at, formal: r.formal != false,
                               current: r.closure_hash == @c AND r.test_version == @v,
                               duration_ms: r.duration_ms, output: SUBSTRING(r.output || '', 0, 1500)}",
                    json!({"t": t, "c": st.and_then(|x| x.closure_hash.clone()), "v": s.version}),
                )
                .await?;
            out.push(json!({
                "id": format!("tests/{t}"), "key": t, "name": s.display_name,
                "language": s.language, "status": st.map(|x| x.status.clone()).unwrap_or_else(|| "not_run".into()),
                "runs": runs,
            }));
        }
        out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(out)
    }

    /// Requirements by kind and by level (spec §7.1: queryable by both).
    pub async fn requirements_by(
        &self,
        kind: Option<&str>,
        level: Option<&str>,
    ) -> Result<Vec<Value>> {
        let rows = self
            .aql(
                // decisions only when asked for by kind; proposed or rejected ones never
                "FOR r IN @@requirements
                   FILTER r.state == null OR r.state == 'accepted'
                   FILTER @kind == null ? r.kind != 'decision' : (r.kind || 'technical') == @kind
                   LET at = (FOR e IN @@applies_to FILTER e._from == r._id RETURN e._to)
                   SORT r.title
                   RETURN {id: r._id, key: r._key, title: r.title, kind: r.kind || 'technical',
                           tags: r.tags || [], attached_to: at}",
                json!({"kind": kind}),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|mut r| {
                let mut levels: Vec<String> = r["attached_to"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|s| level_of(s).to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let rank = |l: &String| {
                    [
                        "project",
                        "context",
                        "container",
                        "component",
                        "code",
                        "test",
                    ]
                    .iter()
                    .position(|x| x == l)
                    .unwrap_or(9)
                };
                levels.sort_by_key(rank);
                levels.dedup();
                r["levels"] = json!(levels);
                r
            })
            .filter(|r| {
                level.is_none_or(|l| {
                    r["levels"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|x| x == l))
                })
            })
            .collect())
    }

    /// Spec §7.4: requirements attached high up whose verifying tests only reach code under
    /// one child: suggest attaching them there instead.
    pub async fn push_down_suggestions(&self) -> Result<Vec<Value>> {
        let v = self.view(None).await?;
        let mut out = Vec::new();
        for (rid, nodes) in &v.applies {
            let verifying: BTreeSet<String> = v
                .verified_by
                .get(rid)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();
            if verifying.is_empty() {
                continue;
            }
            for n in nodes {
                let lvl = level_of(n);
                if !matches!(lvl, "project" | "context" | "container") {
                    continue;
                }
                let kids: Vec<String> = if n == &v.project_node {
                    v.tree
                        .parent
                        .iter()
                        .filter(|(_, p)| *p == n)
                        .map(|(c, _)| c.clone())
                        .collect()
                } else {
                    v.tree.child_nodes(n)
                };
                if kids.len() < 2 {
                    continue;
                }
                let used: Vec<&String> = kids
                    .iter()
                    .filter(|k| {
                        let beneath = v.reach(std::slice::from_ref(*k), false);
                        let tw: BTreeSet<String> = v
                            .tested_with(&beneath)
                            .into_iter()
                            .map(|t| format!("tests/{t}"))
                            .collect();
                        verifying
                            .iter()
                            .any(|t| beneath.contains(t) || tw.contains(t))
                    })
                    .collect();
                if used.len() == 1 {
                    out.push(json!({"requirement": rid, "attached_to": n, "suggest": used[0]}));
                }
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ use cases

    pub async fn create_use_case(&self, mut doc: Value, by: &str) -> Result<Value> {
        let name = doc["name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| StoreError::Invalid("a use case needs a name".into()))?
            .to_string();
        if doc.get("_key").is_none() {
            doc["_key"] = json!(self.db.pkey(&crate::slug(&name))?);
        } else if let Some(k) = doc["_key"].as_str() {
            doc["_key"] = json!(self.db.pkey(k)?);
        }
        doc["state"] = json!("draft");
        doc["created_at"] = json!(now_iso());
        doc["created_by"] = json!(by);
        let rels: Vec<String> = doc["requirements"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let tests: Vec<String> = doc["tests"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(m) = doc.as_object_mut() {
            m.remove("requirements");
            m.remove("tests");
        }
        let created = self.db.insert("use_cases", &doc, None).await.map_err(|e| {
            if e.is_unique_violation() {
                StoreError::Conflict(format!("use case `{name}` exists"))
            } else {
                e
            }
        })?;
        let id = created["_id"].as_str().unwrap_or_default().to_string();
        self.own(&id).await?;
        for r in rels {
            self.link_use_case(&id, "relates_to", &self.db.pid("requirements", key_of(&r))?)
                .await?;
        }
        for t in tests {
            let tid = self.symbol_id(&t).await?;
            self.link_use_case(&id, "verified_by", &tid).await?;
        }
        self.get_use_case(key_of(&id)).await
    }

    /// Link a use case to a requirement it exercises (`relates_to`) or a test that proves it
    /// (`verified_by`).
    pub async fn link_use_case(&self, uc_id: &str, coll: &str, to: &str) -> Result<()> {
        let coll = if coll == "expected_by" {
            "verified_by"
        } else {
            coll
        };
        if !matches!(coll, "relates_to" | "verified_by") {
            return Err(StoreError::Invalid(format!(
                "{coll} is not a use-case edge"
            )));
        }
        let to = if coll == "verified_by" {
            self.symbol_id(to).await?
        } else {
            self.db.qid(to)?
        };
        self.get_node(&to).await?;
        self.db
            .insert_ignore(
                coll,
                &json!({"_key": edge_key(coll, uc_id, &to), "_from": uc_id, "_to": to}),
                None,
            )
            .await
    }

    pub async fn get_use_case(&self, key: &str) -> Result<Value> {
        let uc = self.db.get("use_cases", key, None).await?;
        let id = uc["_id"].clone();
        let rels = self
            .aql(
                "FOR e IN @@relates_to FILTER e._from == @u LET r = DOCUMENT(e._to)
                   RETURN {id: r._id, key: r._key, title: r.title, kind: r.kind || 'technical'}",
                json!({"u": id}),
            )
            .await?;
        let tests = self
            .aql(
                "FOR e IN @@verified_by FILTER e._from == @u LET t = DOCUMENT(e._to)
                   RETURN {id: t._id, key: t._key, name: t.display_name}",
                json!({"u": id}),
            )
            .await?;
        let mut out = uc;
        out["requirements"] = json!(rels);
        out["tests"] = json!(tests);
        Ok(out)
    }

    pub async fn list_use_cases(&self) -> Result<Vec<Value>> {
        let keys = self
            .aql(
                "FOR u IN @@use_cases SORT u.created_at RETURN u._key",
                json!({}),
            )
            .await?;
        let mut out = Vec::new();
        for k in keys.iter().filter_map(Value::as_str) {
            out.push(self.get_use_case(k).await?);
        }
        Ok(out)
    }

    pub async fn update_use_case(&self, key: &str, patch: &Value) -> Result<Value> {
        let mut p = patch.clone();
        if let Some(m) = p.as_object_mut() {
            m.retain(|k, _| {
                matches!(
                    k.as_str(),
                    "name" | "description" | "expected_outcome" | "actor"
                )
            });
        }
        p["updated_at"] = json!(now_iso());
        self.db.patch("use_cases", key, &p, None).await?;
        self.get_use_case(key).await
    }

    /// Move a use case between `draft` and `live` (spec §15.2). With `use_case_check` on (the
    /// default), going live first passes an architect agent's contradiction check against the
    /// live use cases (spec §15.3): the use case waits in `checking`.
    pub async fn set_use_case_state(&self, key: &str, state: &str, by: &str) -> Result<Value> {
        if !matches!(state, "draft" | "live") {
            return Err(StoreError::Invalid(
                "use case state is draft or live".into(),
            ));
        }
        let uc = self.get_use_case(key).await?;
        if state == "live" && uc["tests"].as_array().is_none_or(|a| a.is_empty()) {
            return Err(StoreError::Invalid(
                "a use case goes live only with at least one test that proves it".into(),
            ));
        }
        let project = self.project_id().unwrap_or_default();
        let check = state == "live"
            && uc["state"] != "live"
            && self
                .setting(&project, "use_case_check")
                .await?
                .as_bool()
                .unwrap_or(true);
        let target_state = if check { "checking" } else { state };
        self.db
            .patch(
                "use_cases",
                key,
                &json!({"state": target_state, "state_changed_at": now_iso(), "state_changed_by": by}),
                None,
            )
            .await?;
        if check {
            let live = self
                .aql(
                    "FOR u IN @@use_cases FILTER u.state == 'live' RETURN {key: u._key, name: u.name, description: u.description}",
                    json!({}),
                )
                .await?;
            self.enqueue(&crate::queue::NewWorkItem {
                project,
                agent_type: "architect".into(),
                kind: "check_use_case".into(),
                target: Some(uc["_id"].as_str().unwrap_or_default().to_string()),
                priority: crate::queue::PRIORITY_NORMAL,
                task: json!({"use_case": uc, "live_use_cases": live,
                             "note": "check for contradictions with the live use cases; answer with use_case_verdict"}),
                requested_by: Some(by.to_string()),
                ..Default::default()
            })
            .await?;
        }
        self.get_use_case(key).await
    }

    /// The architect's contradiction check result.
    pub async fn use_case_verdict(
        &self,
        key: &str,
        ok: bool,
        conflicts: Value,
        notes: &str,
        by: &str,
    ) -> Result<Value> {
        let uc = self.get_use_case(key).await?;
        if uc["state"] != "checking" {
            return Err(StoreError::Conflict(format!(
                "use case {key} is not waiting for a check"
            )));
        }
        self.db
            .patch(
                "use_cases",
                key,
                &json!({"state": if ok { "live" } else { "draft" },
                        "check": {"ok": ok, "conflicts": conflicts, "notes": notes, "by": by, "at": now_iso()},
                        "state_changed_at": now_iso()}),
                None,
            )
            .await?;
        self.get_use_case(key).await
    }

    /// Apply an import agent's C4 mapping (spec §13.1).
    pub async fn apply_import_mapping(
        &self,
        mapping: &Value,
        work_item: &str,
        by: &str,
    ) -> Result<Value> {
        crate::import::apply_mapping(self, mapping, work_item, by).await
    }

    // ------------------------------------------------------------------ lifecycle

    /// Lifecycle per node with roll-up (spec §14.4), against the current project version.
    ///
    /// Code: planned (not in the current project version) → in_build (a candidate version is
    /// pending or work is active) → ready_for_test (in the project version, tests exist) →
    /// live (its tests pass). An explicit `lifecycle: in_build` on the node (rework started)
    /// wins over the derived state. Parents roll up their children (`roll_up`), and a parent is
    /// not ready for test while a child is planned or in build unless
    /// `parent_ready_requires_children` is false.
    pub async fn node_lifecycle(&self) -> Result<BTreeMap<String, Value>> {
        let v = self.view(None).await?;
        let health = crate::queries::health_of(&v);
        let leaves = self
            .aql(
                "FOR s IN UNION((FOR x IN @@code RETURN x), (FOR x IN @@tests RETURN x))
                   LET cand = LENGTH(FOR e IN @@has_version FILTER e._from == s._id AND e.state == 'candidate'
                                       AND e.work_item != null RETURN 1)
                   RETURN {id: s._id, cand, explicit: s.lifecycle, test: s.kind == 'test'}",
                json!({}),
            )
            .await?;
        let mut state: BTreeMap<String, String> = BTreeMap::new();
        let tests: BTreeSet<String> = leaves
            .iter()
            .filter(|s| s["test"] == true)
            .filter_map(|s| s["id"].as_str().map(str::to_string))
            .collect();
        for s in &leaves {
            let id = s["id"].as_str().unwrap_or_default().to_string();
            let in_tree = v.tree.version(&id).is_some();
            let h = health
                .get(&id)
                .map(|h| h["health"].clone())
                .unwrap_or(Value::Null);
            let n_tests = health
                .get(&id)
                .and_then(|h| h["tests"].as_u64())
                .unwrap_or(0);
            let derived = if s["explicit"] == "in_build" || s["cand"].as_u64().unwrap_or(0) > 0 {
                "in_build"
            } else if !in_tree {
                "planned"
            } else if h == "pass" {
                "live"
            } else if n_tests > 0 {
                "ready_for_test"
            } else {
                "in_build"
            };
            state.insert(id, derived.to_string());
        }
        let mut children = self.identity_children().await?;
        children.retain(|p, _| !p.starts_with("projects/"));
        let explicit = self
            .aql(
                "FOR n IN UNION((FOR x IN @@contexts RETURN x), (FOR x IN @@containers RETURN x),
                                (FOR x IN @@components RETURN x))
                   RETURN {id: n._id, lifecycle: n.lifecycle}",
                json!({}),
            )
            .await?;
        let strict = match self.project_id() {
            Some(p) => self
                .setting(&p, "parent_ready_requires_children")
                .await
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            None => true,
        };
        #[allow(clippy::too_many_arguments)]
        fn resolve(
            n: &str,
            children: &BTreeMap<String, Vec<String>>,
            state: &mut BTreeMap<String, String>,
            explicit: &BTreeMap<String, String>,
            tests: &BTreeSet<String>,
            strict: bool,
            depth: usize,
        ) -> String {
            if let Some(s) = state.get(n) {
                return s.clone();
            }
            let kids: Vec<String> = children
                .get(n)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|k| !tests.contains(k))
                .collect();
            let ks: Vec<String> = if depth < 12 {
                kids.iter()
                    .map(|k| resolve(k, children, state, explicit, tests, strict, depth + 1))
                    .collect()
            } else {
                vec![]
            };
            let refs: Vec<&str> = ks.iter().map(String::as_str).collect();
            let mut s = roll_up(&refs).to_string();
            if !strict && s == "in_build" && refs.iter().any(|x| rank(x) >= 2) {
                s = "ready_for_test".into();
            }
            if explicit.get(n).map(String::as_str) == Some("in_build") && rank(&s) > 1 {
                s = "in_build".into();
            }
            state.insert(n.to_string(), s.clone());
            s
        }
        let exp: BTreeMap<String, String> = explicit
            .iter()
            .filter_map(|e| {
                Some((
                    e["id"].as_str()?.to_string(),
                    e["lifecycle"].as_str()?.to_string(),
                ))
            })
            .collect();
        let mut out = BTreeMap::new();
        for e in &explicit {
            let id = e["id"].as_str().unwrap_or_default().to_string();
            resolve(&id, &children, &mut state, &exp, &tests, strict, 0);
        }
        for (id, s) in &state {
            let own = if matches!(level_of(id), "code" | "test") {
                s.clone()
            } else {
                exp.get(id).cloned().unwrap_or_else(|| s.clone())
            };
            out.insert(id.clone(), json!({"state": s, "own": own}));
        }
        Ok(out)
    }

    /// Mark a node as in build (rework started) or clear the flag.
    pub async fn set_lifecycle_flag(&self, node_id: &str, in_build: bool) -> Result<()> {
        let node_id = &self.db.qid(node_id)?;
        let (coll, key) = node_id
            .split_once('/')
            .ok_or_else(|| StoreError::Invalid(format!("bad node id {node_id}")))?;
        if !(C4_COLLECTIONS.contains(&coll) || coll == "code" || coll == "tests") {
            return Err(StoreError::Invalid(format!("{coll} has no lifecycle")));
        }
        self.db
            .patch(
                coll,
                key,
                &json!({"lifecycle": if in_build { json!("in_build") } else { Value::Null }}),
                None,
            )
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roll_up_rules() {
        assert_eq!(roll_up(&[]), "planned");
        assert_eq!(roll_up(&["planned", "planned"]), "planned");
        assert_eq!(roll_up(&["planned", "live"]), "in_build");
        assert_eq!(roll_up(&["live", "ready_for_test"]), "ready_for_test");
        assert_eq!(roll_up(&["live", "live"]), "live");
        assert_eq!(roll_up(&["in_build", "live"]), "in_build");
    }

    #[test]
    fn c4_definitions_ignore_bookkeeping() {
        let a = c4_definition(
            "components",
            &json!({"_key": "x", "title": "X", "description": "d", "created_at": "t1"}),
        );
        let b = c4_definition(
            "components",
            &json!({"_key": "y", "title": "X", "description": "d", "created_at": "t2"}),
        );
        assert_eq!(c4_hash(&a), c4_hash(&b));
        let c = c4_definition("containers", &json!({"title": "X", "description": "d"}));
        assert_ne!(c4_hash(&a), c4_hash(&c));
    }

    #[test]
    fn levels() {
        assert_eq!(level_of("code/p.x"), "code");
        assert_eq!(level_of("tests/p.t"), "test");
        assert_eq!(level_of("projects/p"), "project");
    }
}
