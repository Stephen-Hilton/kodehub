//! Releases and environments (doc 06). A release is an immutable manifest (symbol → version
//! hash plus the code edges at cut time). Promotion moves `currently_running` in one
//! transaction; code never links to environments directly.

use std::collections::BTreeMap;

use kodehub_core::model::now_iso;
use kodehub_core::{CodeEdge, CodeGraph, CodeSymbol};
use serde_json::{json, Value};

use crate::store::key_of;
use crate::{Result, Store, StoreError};

impl Store {
    /// Cut a release of the current graph: manifest + `included_in` edges, one transaction.
    pub async fn cut_release(
        &self,
        name: Option<&str>,
        notes: &str,
        by: &str,
        build_targets: &[String],
    ) -> Result<Value> {
        // graph and trace snapshot must come from the same moment (final review #10)
        let _write = self.write_lock.lock().await;
        let g = self.load_code_graph().await?;
        let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
        let key = format!(
            "rel-{ts}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..4]
        );
        let name = name
            .map(String::from)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("release {ts}"));
        let manifest: Vec<Value> = g
            .symbols
            .values()
            .map(|s| json!({"symbol": s.id, "display_name": s.display_name, "kind": s.kind, "language": s.language, "version": s.version}))
            .collect();
        let closures: BTreeMap<String, String> = g.all_closure_hashes()?;
        // context snapshot so release gates evaluate the release, not the working graph (M6 #6)
        let trace = self
            .aql(
                "RETURN {
                   satisfies: (FOR e IN @@satisfies RETURN [e._from, e._to]),
                   verifies: (FOR e IN @@verifies RETURN [e._from, e._to]),
                   contains: (FOR e IN @@contains RETURN [e._from, e._to]),
                   includes: (FOR e IN @@includes RETURN [e._from, e._to]),
                   requirements: (FOR r IN @@requirements RETURN {key: r._key, revision: r.revision, at_risk: r.at_risk == true})
                 }",
                json!({}),
            )
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({}));
        let doc = json!({
            "_key": key,
            "name": name,
            "notes": notes,
            "created_at": now_iso(),
            "created_by": by,
            "manifest": manifest,
            "graph": {"symbols": g.symbols.values().collect::<Vec<_>>(), "edges": g.edges},
            "root_closures": closures,
            "trace": trace,
            "build_targets": build_targets,
            "immutable": true,
        });
        let trx = self.db.begin(&["releases", "included_in"], &[]).await?;
        let res: Result<Value> = async {
            let d = self.db.insert("releases", &doc, Some(&trx)).await?;
            let mut versions: Vec<&String> = g.symbols.values().map(|s| &s.version).collect();
            versions.sort();
            versions.dedup();
            for v in versions {
                self.db
                    .insert("included_in", &json!({"_from": format!("versions/{v}"), "_to": format!("releases/{key}")}), Some(&trx))
                    .await?;
            }
            Ok(d)
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

    pub async fn get_release(&self, key: &str) -> Result<Value> {
        self.db.get("releases", key_of(key), None).await
    }

    pub async fn list_releases(&self) -> Result<Vec<Value>> {
        self.aql(
            "FOR r IN @@releases SORT r.created_at DESC
             LET envs = (FOR e IN @@currently_running FILTER e._to == r._id RETURN PARSE_IDENTIFIER(e._from).key)
             LET deployed = (FOR e IN @@deployed_to FILTER e._from == r._id SORT e.deployed_at
                             RETURN {environment: PARSE_IDENTIFIER(e._to).key, at: e.deployed_at, by: e.deployed_by})
             RETURN MERGE(UNSET(r, 'graph', 'root_closures'), {symbols: LENGTH(r.manifest), running_in: envs, deployments: deployed})",
            json!({}),
        )
        .await
    }

    /// Diff = set difference of version hashes per symbol.
    pub async fn diff_releases(&self, a: &str, b: &str) -> Result<Value> {
        let ra = self.get_release(a).await?;
        let rb = self.get_release(b).await?;
        let index = |r: &Value| -> BTreeMap<String, Value> {
            r["manifest"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|m| (m["symbol"].as_str().unwrap_or_default().to_string(), m))
                .collect()
        };
        let (ia, ib) = (index(&ra), index(&rb));
        let edges_of = |r: &Value| -> BTreeMap<String, Vec<String>> {
            let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for e in r["graph"]["edges"].as_array().cloned().unwrap_or_default() {
                m.entry(e["from"].as_str().unwrap_or_default().to_string())
                    .or_default()
                    .push(format!(
                        "{}:{}:{}",
                        e["kind"].as_str().unwrap_or(""),
                        e["to"].as_str().unwrap_or(""),
                        e["pinned_version"].as_str().unwrap_or("")
                    ));
            }
            for v in m.values_mut() {
                v.sort();
            }
            m
        };
        let vis_of = |r: &Value| -> BTreeMap<String, Value> {
            r["graph"]["symbols"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|s| {
                    (
                        s["id"].as_str().unwrap_or_default().to_string(),
                        s["visibility"].clone(),
                    )
                })
                .collect()
        };
        let (ea, eb, va, vb) = (edges_of(&ra), edges_of(&rb), vis_of(&ra), vis_of(&rb));
        let mut added = vec![];
        let mut removed = vec![];
        let mut changed = vec![];
        for (k, m) in &ib {
            match ia.get(k) {
                None => added.push(m.clone()),
                Some(o) => {
                    // anything that changes rendered code counts (review M6 #17)
                    let mut reasons = Vec::new();
                    if o["version"] != m["version"] {
                        reasons.push("version");
                    }
                    if o["display_name"] != m["display_name"] {
                        reasons.push("renamed");
                    }
                    if va.get(k) != vb.get(k) {
                        reasons.push("visibility");
                    }
                    if ea.get(k) != eb.get(k) {
                        reasons.push("dependencies");
                    }
                    if !reasons.is_empty() {
                        changed.push(json!({"symbol": k, "display_name": m["display_name"], "from": o["version"],
                                            "to": m["version"], "reasons": reasons}));
                    }
                }
            }
        }
        for (k, m) in &ia {
            if !ib.contains_key(k) {
                removed.push(m.clone());
            }
        }
        Ok(
            json!({"from": key_of(a), "to": key_of(b), "added": added, "removed": removed, "changed": changed}),
        )
    }

    /// Reconstruct a release's code graph.
    pub async fn release_graph(&self, key: &str) -> Result<CodeGraph> {
        let r = self.get_release(key).await?;
        let mut g = CodeGraph::default();
        let syms: Vec<CodeSymbol> = serde_json::from_value(r["graph"]["symbols"].clone())
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        let edges: Vec<CodeEdge> = serde_json::from_value(r["graph"]["edges"].clone())
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        for s in syms {
            g.symbols.insert(s.id.clone(), s);
        }
        g.edges = edges;
        let mut needed: Vec<String> = g.symbols.values().map(|s| s.version.clone()).collect();
        needed.extend(g.edges.iter().filter_map(|e| e.pinned_version.clone()));
        needed.sort();
        needed.dedup();
        g.versions = self.load_versions(&needed).await?;
        Ok(g)
    }

    /// Coverage and orphans evaluated on a release's own snapshot (code graph + trace edges +
    /// test results at the release's closure hashes), so gates are reproducible per release
    /// (review M6 #6).
    pub async fn release_trace_report(&self, key: &str) -> Result<Value> {
        use kodehub_core::model::{CodeEdgeKind, Kind};
        use std::collections::BTreeSet;
        let r = self.get_release(key).await?;
        let g = self.release_graph(key).await?;
        let pairs = |f: &str| -> Vec<(String, String)> {
            r["trace"][f]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string())))
                .collect()
        };
        let (sat, ver, con) = (pairs("satisfies"), pairs("verifies"), pairs("contains"));
        let sym = |id: &str| id.strip_prefix("symbols/").map(String::from);
        let mut adj: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
        for (f, t) in &con {
            adj.entry(f.clone()).or_default().push((t.clone(), false));
        }
        for e in &g.edges {
            adj.entry(format!("symbols/{}", e.from)).or_default().push((
                format!("symbols/{}", e.to),
                e.kind == CodeEdgeKind::TestedBy,
            ));
        }
        let reach = |starts: &[String], follow_tests: bool| -> BTreeSet<String> {
            let mut seen: BTreeSet<String> = starts.iter().cloned().collect();
            let mut q: Vec<String> = starts.to_vec();
            while let Some(n) = q.pop() {
                for (t, tb) in adj.get(&n).cloned().unwrap_or_default() {
                    if (!tb || follow_tests) && seen.insert(t.clone()) {
                        q.push(t);
                    }
                }
            }
            seen
        };
        let mut status: BTreeMap<String, String> = BTreeMap::new();
        for s in g.symbols.values().filter(|s| s.kind == Kind::Test) {
            let closure = g.closure_hash(&s.id)?;
            let rows = self
                .aql(
                    "FOR r IN @@test_runs FILTER r.test_version == @v AND r.closure_hash == @c SORT r.ran_at DESC LIMIT 1 RETURN r.status",
                    json!({"v": s.version, "c": closure}),
                )
                .await?;
            status.insert(
                s.id.clone(),
                rows.first()
                    .and_then(Value::as_str)
                    .unwrap_or("not_run")
                    .to_string(),
            );
        }
        let mut failing = Vec::new();
        let mut unverified = Vec::new();
        for req in r["trace"]["requirements"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let rid = format!("requirements/{}", req["key"].as_str().unwrap_or(""));
            let starts: Vec<String> = sat
                .iter()
                .filter(|(_, t)| *t == rid)
                .map(|(f, _)| f.clone())
                .collect();
            let beneath = reach(&starts, false);
            let mut tests: BTreeSet<String> = ver
                .iter()
                .filter(|(_, t)| *t == rid)
                .filter_map(|(f, _)| sym(f))
                .collect();
            for n in &beneath {
                for (t, tb) in adj.get(n).cloned().unwrap_or_default() {
                    if tb {
                        if let Some(k) = sym(&t) {
                            tests.insert(k);
                        }
                    }
                }
            }
            let st: Vec<&str> = tests
                .iter()
                .filter(|t| g.symbols.contains_key(*t))
                .map(|t| status.get(t).map(String::as_str).unwrap_or("not_run"))
                .collect();
            if st.contains(&"fail") {
                failing.push(req["key"].clone());
            } else if st.is_empty() || st.iter().any(|s| *s != "pass") {
                unverified.push(req["key"].clone());
            }
        }
        let mut starts: Vec<String> = sat.iter().map(|(f, _)| f.clone()).collect();
        starts.extend(ver.iter().map(|(f, _)| f.clone()));
        let reached = reach(&starts, true);
        let orphans: Vec<String> = g
            .symbols
            .keys()
            .filter(|k| !reached.contains(&format!("symbols/{k}")))
            .cloned()
            .collect();
        Ok(
            json!({"failing_requirements": failing, "unverified_requirements": unverified, "orphans": orphans}),
        )
    }

    pub async fn running_release(&self, env: &str) -> Result<Option<Value>> {
        let rows = self
            .aql(
                "FOR e IN @@currently_running FILTER e._from == CONCAT(@pfx, 'environments/', @env) RETURN MERGE(UNSET(DOCUMENT(e._to), 'graph', 'root_closures'), {since: e.since})",
                json!({"env": env}),
            )
            .await?;
        Ok(rows.into_iter().next())
    }

    pub async fn release_graph_for_env(&self, env: &str) -> Result<Option<(String, CodeGraph)>> {
        match self.running_release(env).await? {
            Some(r) => {
                let k = r["_key"].as_str().unwrap_or_default().to_string();
                Ok(Some((k.clone(), self.release_graph(&k).await?)))
            }
            None => Ok(None),
        }
    }

    /// Point `env` at `release`: old `currently_running` → `previously_ran {from, until}`,
    /// new `currently_running`, and a `deployed_to` record. One transaction. Gates are the
    /// caller's job (environment agents).
    pub async fn deploy(&self, release: &str, env: &str, by: &str, gate: Value) -> Result<Value> {
        let _write = self.write_lock.lock().await;
        self.deploy_locked(release, env, by, gate, false).await
    }

    async fn deploy_locked(
        &self,
        release: &str,
        env: &str,
        by: &str,
        gate: Value,
        rollback: bool,
    ) -> Result<Value> {
        let rel = key_of(release).to_string();
        self.get_release(&rel).await?;
        self.db.get("environments", env, None).await?;
        let env_id = format!("environments/{env}");
        let rel_id = format!("releases/{rel}");
        let now = now_iso();
        let trx = self
            .db
            .begin(&["currently_running", "previously_ran", "deployed_to"], &[])
            .await?;
        let res: Result<()> = async {
            let cur = self
                .db
                .aql("FOR e IN @@currently_running FILTER e._from == @env RETURN e", json!({"env": env_id}), Some(&trx))
                .await?;
            for c in &cur {
                if c["_to"] == json!(rel_id) {
                    return Err(StoreError::Conflict(format!("{rel} is already running in {env}")));
                }
                self.db.remove("currently_running", c["_key"].as_str().unwrap_or_default(), Some(&trx)).await?;
                // a release rolled away from is never a rollback target again (review M6 #8)
                self.db
                    .insert("previously_ran", &json!({"_from": env_id, "_to": c["_to"], "from": c["since"], "until": now, "rolled_back": rollback}), Some(&trx))
                    .await?;
            }
            self.db
                .insert("currently_running", &json!({"_from": env_id, "_to": rel_id, "since": now, "by": by}), Some(&trx))
                .await
                .map_err(|e| if e.is_unique_violation() { StoreError::Invariant(format!("{env} already has a currently_running edge")) } else { e })?;
            self.db
                .insert("deployed_to", &json!({"_from": rel_id, "_to": env_id, "deployed_at": now, "deployed_by": by, "gate": gate}), Some(&trx))
                .await?;
            Ok(())
        }
        .await;
        match res {
            Ok(()) => {
                self.db.commit(&trx).await?;
                Ok(json!({"environment": env, "release": rel, "deployed_at": now}))
            }
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                Err(e)
            }
        }
    }

    /// Walk back: point `currently_running` at the most recent earlier release that is not the
    /// current one and was not itself rolled away from, so repeated rollbacks keep going back
    /// (review M6 #8).
    pub async fn rollback(&self, env: &str, by: &str) -> Result<Value> {
        let _write = self.write_lock.lock().await;
        let prev = self
            .aql(
                "LET envid = CONCAT(@pfx, 'environments/', @env)
                 LET cur = FIRST(FOR e IN @@currently_running FILTER e._from == envid RETURN e._to)
                 // a release rolled away from is skipped only until it is deployed again (final #13)
                 FOR e IN @@previously_ran FILTER e._from == envid AND e._to != cur
                   LET lastdep = MAX(FOR d IN @@deployed_to FILTER d._from == e._to AND d._to == envid RETURN d.deployed_at)
                   FILTER LENGTH(FOR b IN @@previously_ran FILTER b._from == envid AND b._to == e._to
                                   AND b.rolled_back == true AND b.until >= lastdep RETURN 1) == 0
                   SORT e.until DESC LIMIT 1 RETURN PARSE_IDENTIFIER(e._to).key",
                json!({"env": env}),
            )
            .await?;
        let Some(rel) = prev.first().and_then(Value::as_str).map(String::from) else {
            return Err(StoreError::Invalid(format!(
                "{env} has no earlier release to roll back to"
            )));
        };
        let mut v = self
            .deploy_locked(&rel, env, by, json!({"rollback": true}), true)
            .await?;
        v["rolled_back_to"] = json!(rel);
        Ok(v)
    }

    pub async fn environments(&self) -> Result<Vec<Value>> {
        self.aql(
            "FOR env IN @@environments SORT env.order
             LET cur = FIRST(FOR e IN @@currently_running FILTER e._from == env._id
                             RETURN {release: PARSE_IDENTIFIER(e._to).key, name: DOCUMENT(e._to).name, since: e.since})
             LET prev = (FOR e IN @@previously_ran FILTER e._from == env._id SORT e.until DESC LIMIT 5
                         RETURN {release: PARSE_IDENTIFIER(e._to).key, from: e.from, until: e.until})
             RETURN MERGE(env, {currently_running: cur, history: prev})",
            json!({}),
        )
        .await
    }
}
