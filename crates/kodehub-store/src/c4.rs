//! The C4 layer of v0.1 (spec §5.1–5.2, §7, §14.4, §15): the philosophy document, versions of
//! contexts / containers / components, requirement kinds and inheritance, per-node requirement
//! counts, use cases, and node lifecycle states with roll-up.

use std::collections::{BTreeMap, BTreeSet};

use kodehub_core::model::now_iso;
use serde_json::{json, Value};

use crate::store::{edge_key, key_of};
use crate::{Result, Store, StoreError};

/// C4 levels above code, top first.
pub const C4_COLLECTIONS: &[&str] = &["contexts", "containers", "components"];

/// The singular philosophy node of a project; also the root of its C4 graph.
pub const PHILOSOPHY_ID: &str = "philosophy/philosophy";

/// Requirement kinds known today; the field is open so the list can grow (spec §7.1).
pub const REQUIREMENT_KINDS: &[&str] = &["business", "technical"];

/// Lifecycle states in order (spec §14.4).
pub const LIFECYCLE: &[&str] = &["planned", "in_build", "ready_for_test", "live"];

fn rank(s: &str) -> usize {
    LIFECYCLE.iter().position(|x| *x == s).unwrap_or(0)
}

pub fn level_of(id: &str) -> &'static str {
    match id.split_once('/').map(|x| x.0) {
        Some("philosophy") => "project",
        Some("contexts") => "context",
        Some("containers") => "container",
        Some("components") => "component",
        Some("symbols") => "code",
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
    // ------------------------------------------------------------------ philosophy

    pub async fn get_philosophy(&self) -> Result<Option<Value>> {
        match self.db.get("philosophy", "philosophy", None).await {
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
                    .patch("philosophy", "philosophy", &patch, None)
                    .await
            }
            None => {
                self.db
                    .insert(
                        "philosophy",
                        &json!({"_key": "philosophy", "title": title.unwrap_or("Philosophy"),
                                "text": text.trim(), "raw_transcript": raw_transcript,
                                "revision": 1, "history": [], "created_at": now,
                                "updated_at": now, "updated_by": by}),
                        None,
                    )
                    .await?;
                self.db.get("philosophy", "philosophy", None).await
            }
        }
    }

    // ------------------------------------------------------------------ C4 versions

    /// Make the node's stable C4 version match its current definition, creating an immutable
    /// `c4_versions` node and moving the SCD edge when it changed. Returns the version hash.
    pub async fn sync_c4_version(&self, collection: &str, key: &str, by: &str) -> Result<String> {
        if !C4_COLLECTIONS.contains(&collection) {
            return Err(StoreError::Invalid(format!(
                "{collection} is not a C4 level"
            )));
        }
        let doc = self.db.get(collection, key, None).await?;
        let def = c4_definition(collection, &doc);
        let hash = c4_hash(&def);
        let node_id = format!("{collection}/{key}");
        if self.stable_version_in(&node_id, None).await?.as_deref() == Some(hash.as_str()) {
            return Ok(hash);
        }
        let trx = self.db.begin(&["c4_versions", "has_version"], &[]).await?;
        let res: Result<()> = async {
            self.db
                .insert_ignore(
                    "c4_versions",
                    &json!({"_key": hash, "content_hash": hash, "definition": def,
                            "created_at": now_iso(), "created_by": by}),
                    Some(&trx),
                )
                .await?;
            self.promote_in(&node_id, "c4_versions", &hash, None, by, &trx)
                .await?;
            Ok(())
        }
        .await;
        match res {
            Ok(()) => {
                self.db.commit(&trx).await?;
                Ok(hash)
            }
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                Err(e)
            }
        }
    }

    /// Sync every context, container and component (after a seed or import).
    pub async fn sync_all_c4_versions(&self, by: &str) -> Result<usize> {
        let mut n = 0;
        for coll in C4_COLLECTIONS {
            let keys = self
                .aql("FOR d IN @@c RETURN d._key", json!({"@c": coll}))
                .await?;
            for k in keys.iter().filter_map(Value::as_str) {
                self.sync_c4_version(coll, k, by).await?;
                n += 1;
            }
        }
        Ok(n)
    }

    // ------------------------------------------------------------------ requirements

    /// Every requirement that applies to a node: its own plus everything inherited from its
    /// ancestors up to the project (spec §7.2), nearest first. Groups are expanded.
    pub async fn requirements_for(&self, node_id: &str) -> Result<Value> {
        let chain = self.ancestors(node_id).await?;
        let mut seen = BTreeSet::new();
        let mut rows = Vec::new();
        for (distance, n) in chain.iter().enumerate() {
            let direct = self
                .aql(
                    "FOR e IN @@satisfies FILTER e._from == @n
                       LET t = DOCUMENT(e._to)
                       LET reqs = STARTS_WITH(e._to, CONCAT(@pfx, 'requirement_groups/'))
                         ? (FOR r IN 1..5 OUTBOUND e._to @@includes
                              FILTER STARTS_WITH(r._id, CONCAT(@pfx, 'requirements/')) RETURN r)
                         : [t]
                       FOR r IN reqs RETURN {r, via: e._to}",
                    json!({"n": n}),
                )
                .await?;
            for row in direct {
                let r = &row["r"];
                let id = r["_id"].as_str().unwrap_or_default().to_string();
                if !seen.insert(id.clone()) {
                    continue;
                }
                rows.push(json!({
                    "id": id, "key": r["_key"], "title": r["title"], "statement": r["statement"],
                    "kind": r["kind"].as_str().unwrap_or("technical"),
                    "attached_to": n, "attached_level": level_of(n),
                    "inherited": distance > 0, "distance": distance, "via": row["via"],
                }));
            }
        }
        Ok(json!({
            "node": node_id, "chain": chain, "requirements": rows,
            "philosophy": self.get_philosophy().await?.map(|p| p["text"].clone()),
        }))
    }

    /// The node itself and its ancestors up to the philosophy root, nearest first.
    pub async fn ancestors(&self, node_id: &str) -> Result<Vec<String>> {
        let mut chain = vec![node_id.to_string()];
        let mut cur = node_id.to_string();
        for _ in 0..12 {
            let parent = self
                .aql(
                    "LET p = FIRST(FOR e IN @@contains FILTER e._to == @n RETURN e._from)
                     LET c = FIRST(FOR e IN @@member_of FILTER e._from == @n RETURN e._to)
                     RETURN p || c",
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
        if !chain.iter().any(|c| c == PHILOSOPHY_ID) && self.get_philosophy().await?.is_some() {
            chain.push(PHILOSOPHY_ID.to_string());
        }
        Ok(chain)
    }

    /// Requirement counts per node (spec §8.3): own and own-plus-children, by kind.
    pub async fn requirement_counts(&self) -> Result<BTreeMap<String, Value>> {
        let sat = self
            .aql(
                "FOR e IN @@satisfies
                   LET reqs = STARTS_WITH(e._to, CONCAT(@pfx, 'requirement_groups/'))
                     ? (FOR r IN 1..5 OUTBOUND e._to @@includes
                          FILTER STARTS_WITH(r._id, CONCAT(@pfx, 'requirements/')) RETURN r)
                     : [DOCUMENT(e._to)]
                   FOR r IN reqs FILTER r != null
                   RETURN {n: e._from, r: r._id, kind: r.kind || 'technical'}",
                json!({}),
            )
            .await?;
        let kids = self
            .aql(
                "FOR e IN UNION((FOR x IN @@contains RETURN {p: x._from, c: x._to}),
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
        let mut own: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
        for s in &sat {
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
            for k in REQUIREMENT_KINDS {
                m.insert((*k).into(), json!(0));
            }
            for (_, kind) in set {
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

    /// Requirements by kind and by level (spec §7.1: queryable by both).
    pub async fn requirements_by(
        &self,
        kind: Option<&str>,
        level: Option<&str>,
    ) -> Result<Vec<Value>> {
        let rows = self
            .aql(
                "FOR r IN @@requirements
                   FILTER @kind == null OR (r.kind || 'technical') == @kind
                   LET at = (FOR e IN @@satisfies FILTER e._to == r._id RETURN e._from)
                   LET via_groups = (FOR g IN 1..5 INBOUND r._id @@includes
                                       FOR e IN @@satisfies FILTER e._to == g._id RETURN e._from)
                   SORT r.title
                   RETURN {id: r._id, key: r._key, title: r.title, kind: r.kind || 'technical',
                           attached_to: UNIQUE(APPEND(at, via_groups))}",
                json!({"kind": kind}),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|mut r| {
                let levels: Vec<String> = r["attached_to"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|s| level_of(s).to_string())
                            .collect()
                    })
                    .unwrap_or_default();
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

    /// Spec §7.4: requirements attached high up whose code reach is confined to one child.
    pub async fn push_down_suggestions(&self) -> Result<Vec<Value>> {
        let rows = self
            .aql(
                "FOR e IN @@satisfies
                   FILTER STARTS_WITH(e._from, CONCAT(@pfx, 'contexts/')) OR STARTS_WITH(e._from, CONCAT(@pfx, 'containers/'))
                     OR e._from == CONCAT(@pfx, 'philosophy/philosophy')
                   LET kids = (FOR c IN 1..1 OUTBOUND e._from @@contains RETURN c._id)
                   FILTER LENGTH(kids) > 1
                   LET verifying = (FOR v IN @@verifies FILTER v._to == e._to
                                      FOR t IN @@tested_by FILTER t._to == v._from RETURN t._from)
                   LET used = UNIQUE(FOR k IN kids
                                FOR x IN 0..10 OUTBOUND k @@contains
                                  FILTER x._id IN verifying RETURN k)
                   FILTER LENGTH(used) == 1
                   RETURN {requirement: e._to, attached_to: e._from, suggest: FIRST(used)}",
                json!({}),
            )
            .await?;
        Ok(rows)
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
            doc["_key"] = json!(crate::slug(&name));
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
        for r in rels {
            self.link_use_case(&id, "relates_to", &format!("requirements/{}", key_of(&r)))
                .await?;
        }
        for t in tests {
            self.link_use_case(&id, "expected_by", &format!("symbols/{}", key_of(&t)))
                .await?;
        }
        self.get_use_case(key_of(&id)).await
    }

    pub async fn link_use_case(&self, uc_id: &str, coll: &str, to: &str) -> Result<()> {
        if !matches!(coll, "relates_to" | "expected_by") {
            return Err(StoreError::Invalid(format!(
                "{coll} is not a use-case edge"
            )));
        }
        self.get_node(to).await?;
        self.db
            .insert_ignore(
                coll,
                &json!({"_key": edge_key(coll, uc_id, to), "_from": uc_id, "_to": to}),
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
                "FOR e IN @@expected_by FILTER e._from == @u LET t = DOCUMENT(e._to)
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

    /// Move a use case between `draft` and `live` (spec §15.2).
    pub async fn set_use_case_state(&self, key: &str, state: &str, by: &str) -> Result<Value> {
        if !matches!(state, "draft" | "live") {
            return Err(StoreError::Invalid(
                "use case state is draft or live".into(),
            ));
        }
        let uc = self.get_use_case(key).await?;
        if state == "live" && uc["tests"].as_array().is_none_or(|a| a.is_empty()) {
            return Err(StoreError::Invalid(
                "a use case goes live only with at least one expected-outcome test".into(),
            ));
        }
        self.db
            .patch(
                "use_cases",
                key,
                &json!({"state": state, "state_changed_at": now_iso(), "state_changed_by": by}),
                None,
            )
            .await?;
        self.get_use_case(key).await
    }

    // ------------------------------------------------------------------ lifecycle

    /// Lifecycle per node with roll-up (spec §14.4).
    ///
    /// Code: planned (no stable version) → in_build (a candidate exists or work is active) →
    /// ready_for_test (stable, tests exist) → live (stable and its tests pass). An explicit
    /// `lifecycle: in_build` on the node (rework started) wins over the derived state.
    /// Parents roll up their children (`roll_up`), and a parent is not ready for test while a
    /// child is planned or in build unless `parent_ready_requires_children` is false.
    pub async fn node_lifecycle(&self) -> Result<BTreeMap<String, Value>> {
        let health = self.node_health().await?;
        let syms = self
            .aql(
                "FOR s IN @@symbols
                   LET stable = LENGTH(FOR e IN @@has_version FILTER e._from == s._id AND e.state == 'stable' RETURN 1) > 0
                   LET cand = LENGTH(FOR e IN @@has_version FILTER e._from == s._id AND e.state == 'candidate' RETURN 1)
                   RETURN {id: s._id, stable, cand, explicit: s.lifecycle}",
                json!({}),
            )
            .await?;
        let mut state: BTreeMap<String, String> = BTreeMap::new();
        for s in &syms {
            let id = s["id"].as_str().unwrap_or_default().to_string();
            let h = health
                .get(&id)
                .map(|h| h["health"].clone())
                .unwrap_or(Value::Null);
            let tests = health
                .get(&id)
                .and_then(|h| h["tests"].as_u64())
                .unwrap_or(0);
            let derived = if s["explicit"] == "in_build" || s["cand"].as_u64().unwrap_or(0) > 0 {
                "in_build"
            } else if s["stable"] != true {
                "planned"
            } else if h == "pass" {
                "live"
            } else if tests > 0 {
                "ready_for_test"
            } else {
                "in_build"
            };
            state.insert(id, derived.to_string());
        }
        let edges = self
            .aql(
                "FOR e IN @@contains FILTER !STARTS_WITH(e._from, CONCAT(@pfx, 'philosophy/')) RETURN {p: e._from, c: e._to}",
                json!({}),
            )
            .await?;
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for e in &edges {
            if let (Some(p), Some(c)) = (e["p"].as_str(), e["c"].as_str()) {
                children.entry(p.into()).or_default().push(c.into());
            }
        }
        let explicit = self
            .aql(
                "FOR n IN UNION((FOR x IN @@contexts RETURN x), (FOR x IN @@containers RETURN x),
                                (FOR x IN @@components RETURN x))
                   RETURN {id: n._id, lifecycle: n.lifecycle}",
                json!({}),
            )
            .await?;
        let strict = self
            .project_id()
            .map(|p| async move { self.setting(&p, "parent_ready_requires_children").await })
            .ok_or(())
            .ok();
        let strict = match strict {
            Some(f) => f.await.ok().and_then(|v| v.as_bool()).unwrap_or(true),
            None => true,
        };
        fn resolve(
            n: &str,
            children: &BTreeMap<String, Vec<String>>,
            state: &mut BTreeMap<String, String>,
            explicit: &BTreeMap<String, String>,
            strict: bool,
            depth: usize,
        ) -> String {
            if let Some(s) = state.get(n) {
                return s.clone();
            }
            let kids = children.get(n).cloned().unwrap_or_default();
            let ks: Vec<String> = if depth < 12 {
                kids.iter()
                    .map(|k| resolve(k, children, state, explicit, strict, depth + 1))
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
            resolve(&id, &children, &mut state, &exp, strict, 0);
        }
        for (id, s) in &state {
            let own = if level_of(id) == "code" {
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
        let (coll, key) = node_id
            .split_once('/')
            .ok_or_else(|| StoreError::Invalid(format!("bad node id {node_id}")))?;
        if !(C4_COLLECTIONS.contains(&coll) || coll == "symbols") {
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
        assert_eq!(level_of("symbols/x"), "code");
        assert_eq!(level_of(PHILOSOPHY_ID), "project");
    }
}
