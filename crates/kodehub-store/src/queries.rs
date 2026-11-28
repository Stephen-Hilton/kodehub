//! Traceability queries (doc 02, docs/ontology.md): coverage, orphans, impact, duplicates and
//! per-node health, always for one project version (default: the current one).
//!
//! Everything is computed from one [`View`]: the project version's tree, its code graph, the
//! requirement links (`applies_to`, `verified_by`) and the latest formal test results at each
//! test's closure hash in that project version.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use kodehub_core::model::{CodeEdgeKind, Kind};
use kodehub_core::CodeGraph;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::tree::TreeIndex;
use crate::{Result, Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestStatus {
    /// `tests/<key>`.
    pub test: String,
    pub name: String,
    pub language: String,
    /// pass | fail | not_run
    pub status: String,
    pub ran_at: Option<String>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequirementCoverage {
    pub id: String,
    pub key: String,
    pub title: String,
    pub statement: String,
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub revision: i64,
    pub at_risk: bool,
    /// pass | fail | at_risk | uncovered
    pub status: String,
    /// Nodes the requirement applies to.
    pub applies_to: Vec<String>,
    pub tests: Vec<TestStatus>,
}

/// Requirements sharing a tag, folded together (all of them must pass).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagCoverage {
    pub tag: String,
    pub status: String,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coverage {
    pub requirements: Vec<RequirementCoverage>,
    pub tags: Vec<TagCoverage>,
    pub summary: BTreeMap<String, usize>,
}

/// The latest formal result of one test at its closure hash in a project version.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestState {
    pub status: String,
    pub ran_at: Option<String>,
    pub duration_ms: Option<i64>,
    /// A build of this test version exists.
    pub built: bool,
    pub closure_hash: Option<String>,
    pub version: String,
}

fn tests_status(tests: &[TestStatus]) -> &'static str {
    if tests.is_empty() {
        "uncovered"
    } else if tests.iter().any(|t| t.status == "fail") {
        "fail"
    } else if tests.iter().any(|t| t.status != "pass") {
        "at_risk"
    } else {
        "pass"
    }
}

/// Combine statuses (worst wins).
pub fn worst(statuses: &[&str]) -> &'static str {
    let rank = |s: &str| match s {
        "fail" => 3,
        "at_risk" => 2,
        "uncovered" => 1,
        _ => 0,
    };
    match statuses.iter().map(|s| rank(s)).max().unwrap_or(1) {
        3 => "fail",
        2 => "at_risk",
        1 => "uncovered",
        _ => "pass",
    }
}

/// The node id of a code-graph key (`tests/…` for tests, `code/…` otherwise).
pub fn node_id(g: &CodeGraph, key: &str) -> String {
    match g.symbols.get(key).map(|s| s.kind) {
        Some(Kind::Test) => format!("tests/{key}"),
        _ => format!("code/{key}"),
    }
}

fn is_leaf(id: &str) -> bool {
    id.starts_with("code/") || id.starts_with("tests/")
}

/// One project version, loaded for traceability.
#[derive(Debug, Clone)]
pub struct View {
    pub pv: String,
    pub project_node: String,
    pub tree: Arc<TreeIndex>,
    pub g: CodeGraph,
    /// Requirement documents by id.
    pub requirements: BTreeMap<String, Value>,
    /// Requirement id → nodes it applies to.
    pub applies: BTreeMap<String, Vec<String>>,
    /// Requirement or use case id → tests verifying it (`tests/<key>`).
    pub verified_by: BTreeMap<String, Vec<String>>,
    /// Test key → latest formal state.
    pub states: BTreeMap<String, TestState>,
}

impl View {
    /// Tests (keys) attached by `tested_with` to any of `nodes` (node ids).
    pub fn tested_with(&self, nodes: &BTreeSet<String>) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let keys: BTreeSet<&str> = nodes
            .iter()
            .filter(|n| is_leaf(n))
            .map(|n| crate::key_of(n))
            .collect();
        for e in &self.g.edges {
            if e.kind == CodeEdgeKind::TestedWith && keys.contains(e.from.as_str()) {
                out.insert(e.to.clone());
            }
        }
        out
    }

    /// Everything beneath `starts`: tree descendants of C4 nodes (the project node means the
    /// whole tree), then build dependencies of code (calls, depends_on, member_of), and, with
    /// `follow_tests` (reachability for orphans), cross-language `binds_to` and tests attached
    /// by `tested_with`. Health and coverage don't cross a binding: a wrapper's health is its
    /// own language's tests (as in v0.1).
    pub fn reach(&self, starts: &[String], follow_tests: bool) -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut todo: Vec<String> = starts.to_vec();
        while let Some(n) = todo.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            if n == self.project_node {
                todo.extend(self.tree.version_of.keys().cloned());
                continue;
            }
            if !is_leaf(&n) {
                todo.extend(self.tree.child_nodes(&n));
                continue;
            }
            let k = crate::key_of(&n);
            for e in self.g.out_edges(k) {
                let follow = match e.kind {
                    CodeEdgeKind::TestedWith | CodeEdgeKind::BindsTo => follow_tests,
                    k => k.is_build_dependency(),
                };
                if follow {
                    todo.push(node_id(&self.g, &e.to));
                }
            }
        }
        seen
    }

    /// Status rows for a set of test keys, sorted by name.
    pub fn test_rows(&self, tests: &BTreeSet<String>) -> Vec<TestStatus> {
        let mut rows: Vec<TestStatus> = tests
            .iter()
            .filter_map(|t| {
                let s = self.g.symbols.get(t)?;
                let st = self.states.get(t);
                Some(TestStatus {
                    test: format!("tests/{t}"),
                    name: s.display_name.clone(),
                    language: s.language.as_str().into(),
                    status: st
                        .map(|x| x.status.clone())
                        .unwrap_or_else(|| "not_run".into()),
                    ran_at: st.and_then(|x| x.ran_at.clone()),
                    duration_ms: st.and_then(|x| x.duration_ms),
                })
            })
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// Tests that check a requirement: those attached to what it applies to (and beneath), and
    /// those verifying it directly.
    pub fn requirement_tests(&self, req: &str) -> BTreeSet<String> {
        let starts = self.applies.get(req).cloned().unwrap_or_default();
        let reach = self.reach(&starts, false);
        let mut tests = self.tested_with(&reach);
        for t in self.verified_by.get(req).into_iter().flatten() {
            tests.insert(crate::key_of(t).to_string());
        }
        tests.retain(|t| self.g.symbols.contains_key(t));
        tests
    }

    /// Every node some requirement reaches (orphans are code and tests outside this set).
    pub fn reached(&self) -> BTreeSet<String> {
        let mut starts: Vec<String> = self.applies.values().flatten().cloned().collect();
        starts.extend(self.verified_by.values().flatten().cloned());
        self.reach(&starts, true)
    }
}

impl Store {
    /// Latest formal results for every test in `g` at its closure hash.
    pub async fn test_states(&self, g: &CodeGraph) -> Result<BTreeMap<String, TestState>> {
        let pairs: Vec<Value> = g
            .symbols
            .values()
            .filter(|s| s.kind == Kind::Test)
            .map(|s| json!({"t": s.id, "v": s.version, "c": g.closure_hash(&s.id).ok()}))
            .collect();
        let rows = self
            .aql(
                "FOR p IN @pairs
                   LET run = p.c == null ? null : FIRST(
                       FOR r IN @@test_logs FILTER r.test_version == p.v AND r.closure_hash == p.c
                         AND r.formal != false AND r.kind != 'run'
                         SORT r.ran_at DESC LIMIT 1 RETURN r)
                   LET built = LENGTH(FOR b IN @@build_of FILTER b._to == CONCAT('test_versions/', p.v) LIMIT 1 RETURN 1) > 0
                   RETURN {t: p.t, v: p.v, c: p.c, status: run ? run.status : 'not_run',
                           ran_at: run.ran_at, duration_ms: run.duration_ms, built}",
                json!({"pairs": pairs}),
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some((
                    r["t"].as_str()?.to_string(),
                    TestState {
                        status: r["status"].as_str().unwrap_or("not_run").into(),
                        ran_at: r["ran_at"].as_str().map(str::to_string),
                        duration_ms: r["duration_ms"].as_i64(),
                        built: r["built"] == true,
                        closure_hash: r["c"].as_str().map(str::to_string),
                        version: r["v"].as_str().unwrap_or_default().into(),
                    },
                ))
            })
            .collect())
    }

    /// Load a project version (`None` = current) for traceability.
    pub async fn view(&self, pv: Option<&str>) -> Result<View> {
        let pv = match pv {
            Some(v) => self.pv_id(v)?,
            None => self.current_version().await?,
        };
        let tree = self.tree(Some(&pv)).await?;
        let g = self.load_code_graph_at(Some(&pv)).await?;
        let mut v = self.view_with(tree, g, None).await?;
        v.states = self.test_states(&v.g).await?;
        Ok(v)
    }

    /// A view of a resolved tree and code graph (inside `trx` when given), without test
    /// results: enough for reachability and orphan checks.
    pub async fn view_with(
        &self,
        tree: Arc<TreeIndex>,
        g: CodeGraph,
        trx: Option<&str>,
    ) -> Result<View> {
        let pv = tree.root.clone();
        let reqs = self
            .db
            .aql(
                // decisions (kind `decision`) record a choice; they are not tested and do not
                // make code reachable
                "FOR r IN @@requirements FILTER (r.state == null OR r.state == 'accepted')
                   AND r.kind != 'decision' RETURN r",
                json!({}),
                trx,
            )
            .await?;
        let mut applies: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for e in self
            .db
            .aql(
                "FOR e IN @@applies_to RETURN {r: e._from, n: e._to}",
                json!({}),
                trx,
            )
            .await?
        {
            if let (Some(r), Some(n)) = (e["r"].as_str(), e["n"].as_str()) {
                applies.entry(r.into()).or_default().push(n.into());
            }
        }
        let mut verified_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for e in self
            .db
            .aql(
                "FOR e IN @@verified_by RETURN {r: e._from, t: e._to}",
                json!({}),
                trx,
            )
            .await?
        {
            if let (Some(r), Some(t)) = (e["r"].as_str(), e["t"].as_str()) {
                verified_by.entry(r.into()).or_default().push(t.into());
            }
        }
        let ids: BTreeSet<&str> = reqs.iter().filter_map(|r| r["_id"].as_str()).collect();
        applies.retain(|r, _| ids.contains(r.as_str()));
        verified_by.retain(|r, _| ids.contains(r.as_str()) || r.starts_with("use_cases/"));
        let project = self.db.require_project()?;
        Ok(View {
            pv,
            project_node: format!("{}/{project}", crate::schema::PROJECTS),
            tree,
            g,
            requirements: reqs
                .into_iter()
                .filter_map(|r| Some((r["_id"].as_str()?.to_string(), r)))
                .collect(),
            applies,
            verified_by,
            states: BTreeMap::new(),
        })
    }

    /// Coverage for every requirement (or one), and per tag.
    pub async fn coverage(&self, requirement: Option<&str>) -> Result<Coverage> {
        let v = self.view(None).await?;
        let only = match requirement {
            Some(r) if r.contains('/') => Some(self.db.qid(r)?),
            Some(r) => Some(format!("requirements/{}", self.db.pkey(r)?)),
            None => None,
        };
        let mut reqs: Vec<RequirementCoverage> = Vec::new();
        for (id, r) in &v.requirements {
            if only.as_ref().is_some_and(|o| o != id) {
                continue;
            }
            if r["kind"] == "decision" {
                continue; // decisions are recorded, not tested
            }
            let tests = v.test_rows(&v.requirement_tests(id));
            let at_risk = r["at_risk"] == true;
            let mut status = tests_status(&tests).to_string();
            if at_risk && status != "fail" {
                status = "at_risk".into();
            }
            reqs.push(RequirementCoverage {
                id: id.clone(),
                key: r["_key"].as_str().unwrap_or_default().into(),
                title: r["title"]
                    .as_str()
                    .or(r["statement"].as_str())
                    .unwrap_or_default()
                    .into(),
                statement: r["statement"].as_str().unwrap_or_default().into(),
                kind: r["kind"].as_str().unwrap_or("technical").into(),
                tags: serde_json::from_value(r["tags"].clone()).unwrap_or_default(),
                revision: r["revision"].as_i64().unwrap_or(1),
                at_risk,
                status,
                applies_to: v.applies.get(id).cloned().unwrap_or_default(),
                tests,
            });
        }
        reqs.sort_by(|a, b| a.title.cmp(&b.title));
        let mut by_tag: BTreeMap<String, Vec<&RequirementCoverage>> = BTreeMap::new();
        for r in &reqs {
            for t in &r.tags {
                by_tag.entry(t.clone()).or_default().push(r);
            }
        }
        let tags = if requirement.is_some() {
            vec![]
        } else {
            by_tag
                .into_iter()
                .map(|(tag, members)| {
                    let st: Vec<&str> = members.iter().map(|m| m.status.as_str()).collect();
                    TagCoverage {
                        tag,
                        status: worst(&st).into(),
                        members: members.iter().map(|m| m.id.clone()).collect(),
                    }
                })
                .collect()
        };
        let mut summary = BTreeMap::new();
        for r in &reqs {
            *summary.entry(r.status.clone()).or_insert(0) += 1;
        }
        Ok(Coverage {
            requirements: reqs,
            tags,
            summary,
        })
    }

    /// Code and tests in the current project version that no requirement reaches.
    pub async fn orphans(&self) -> Result<Vec<Value>> {
        let v = self.view(None).await?;
        Ok(orphans_of(&v))
    }

    /// Everything at risk if a code node changes: dependants upward, the tests that exercise
    /// it, and the requirements those reach.
    pub async fn impact(&self, symbol: &str) -> Result<Value> {
        let sid = self.symbol_id(symbol).await?;
        let v = self.view(None).await?;
        let key = crate::key_of(&sid).to_string();
        // dependants: inbound build edges, then the containing C4 nodes
        let mut up: BTreeSet<String> = BTreeSet::new();
        let mut todo = vec![key.clone()];
        while let Some(k) = todo.pop() {
            if !up.insert(k.clone()) {
                continue;
            }
            for e in v.g.in_edges(&k) {
                if e.kind.is_build_dependency() {
                    todo.push(e.from.clone());
                }
            }
        }
        let mut nodes: BTreeSet<String> = BTreeSet::new();
        for k in &up {
            for a in v.tree.ancestors(&node_id(&v.g, k)).into_iter().skip(1) {
                if !is_leaf(&a) && a != v.project_node {
                    nodes.insert(a);
                }
            }
        }
        let up_ids: BTreeSet<String> = up.iter().map(|k| node_id(&v.g, k)).collect();
        let mut all: BTreeSet<String> = up_ids.clone();
        all.extend(nodes.iter().cloned());
        all.insert(v.project_node.clone());
        let tests: BTreeSet<String> = v
            .tested_with(&up_ids)
            .into_iter()
            .chain(
                up.iter()
                    .filter(|k| v.g.symbols.get(*k).is_some_and(|s| s.kind == Kind::Test))
                    .cloned(),
            )
            .collect();
        let test_ids: BTreeSet<String> = tests.iter().map(|t| format!("tests/{t}")).collect();
        let mut hit: Vec<Value> = Vec::new();
        let mut tags: BTreeSet<String> = BTreeSet::new();
        for (rid, r) in &v.requirements {
            let direct = v
                .applies
                .get(rid)
                .is_some_and(|ns| ns.iter().any(|n| all.contains(n)));
            let via_tests = v
                .verified_by
                .get(rid)
                .is_some_and(|ts| ts.iter().any(|t| test_ids.contains(t)));
            if direct || via_tests {
                for t in r["tags"].as_array().into_iter().flatten() {
                    if let Some(t) = t.as_str() {
                        tags.insert(t.to_string());
                    }
                }
                hit.push(json!({"id": rid, "title": r["title"], "statement": r["statement"]}));
            }
        }
        hit.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
        let symbols: Vec<Value> = up
            .iter()
            .filter_map(|k| {
                let s = v.g.symbols.get(k)?;
                Some(
                    json!({"id": node_id(&v.g, k), "display_name": s.display_name,
                            "kind": s.kind, "language": s.language}),
                )
            })
            .collect();
        let mut node_rows = Vec::new();
        for n in &nodes {
            let d = self.get_node(n).await?;
            node_rows.push(json!({"id": n, "title": d["title"].as_str().or(d["name"].as_str())}));
        }
        Ok(json!({
            "symbol": sid, "symbols": symbols, "nodes": node_rows,
            "tests": v.test_rows(&tests), "requirements": hit, "tags": tags,
        }))
    }

    /// Exact duplicates: code whose versions in the current project version share a content
    /// hash.
    pub async fn duplicates(&self) -> Result<Vec<Value>> {
        let v = self.view(None).await?;
        let mut by_hash: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for s in v.g.symbols.values().filter(|s| s.kind != Kind::Test) {
            let Some(vid) = v.tree.version(&node_id(&v.g, &s.id)) else {
                continue;
            };
            if let Some(h) = v.tree.content_hash.get(vid) {
                by_hash.entry(h.clone()).or_default().push(
                    json!({"id": node_id(&v.g, &s.id), "display_name": s.display_name,
                           "language": s.language}),
                );
            }
        }
        Ok(by_hash
            .into_iter()
            .filter(|(_, ss)| ss.len() > 1)
            .map(|(h, mut ss)| {
                ss.sort_by(|a, b| a["display_name"].as_str().cmp(&b["display_name"].as_str()));
                json!({"content_hash": h, "symbols": ss})
            })
            .collect())
    }

    /// Near duplicates (doc 02, M5+): code pairs of the same language and kind whose bodies have
    /// Jaccard similarity ≥ `threshold` over token 3-grams (exact duplicates excluded). A
    /// deterministic stand-in for embeddings (decisions.md D-035).
    pub async fn near_duplicates(&self, threshold: f64) -> Result<Vec<Value>> {
        let g = self.load_code_graph().await?;
        let syms: Vec<_> = g
            .symbols
            .values()
            .filter(|s| s.kind != Kind::Test)
            .collect();
        let content = |s: &kodehub_core::CodeSymbol| -> Option<String> {
            g.version(&s.version)
                .ok()
                .map(|v| format!("{}\n{}", v.signature, v.body))
        };
        let sh: Vec<_> = syms
            .iter()
            .map(|s| {
                content(s)
                    .map(|c| kodehub_core::infer::shingles(s.language, &c))
                    .unwrap_or_default()
            })
            .collect();
        let mut out = Vec::new();
        for i in 0..syms.len() {
            for j in i + 1..syms.len() {
                let (a, b) = (syms[i], syms[j]);
                if a.language != b.language || a.kind != b.kind || content(a) == content(b) {
                    continue;
                }
                let score = kodehub_core::infer::similarity(&sh[i], &sh[j]);
                if score >= threshold {
                    out.push(
                        json!({"a": {"id": node_id(&g, &a.id), "display_name": a.display_name},
                                    "b": {"id": node_id(&g, &b.id), "display_name": b.display_name},
                                    "similarity": (score * 1000.0).round() / 1000.0}),
                    );
                }
            }
        }
        out.sort_by(|x, y| {
            y["similarity"]
                .as_f64()
                .partial_cmp(&x["similarity"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(out)
    }

    /// Health of every node in the current project version, for colouring:
    /// fail > at_risk > pass; uncovered (no tests beneath); orphan (no requirement reaches it).
    pub async fn node_health(&self) -> Result<BTreeMap<String, Value>> {
        let v = self.view(None).await?;
        Ok(health_of(&v))
    }
}

/// Orphans of a view.
pub fn orphans_of(v: &View) -> Vec<Value> {
    let reached = v.reached();
    let mut out: Vec<Value> =
        v.g.symbols
            .values()
            .filter(|s| !reached.contains(&node_id(&v.g, &s.id)))
            .map(|s| {
                json!({"id": node_id(&v.g, &s.id), "key": s.id, "display_name": s.display_name,
                   "kind": s.kind, "language": s.language})
            })
            .collect();
    out.sort_by(|a, b| a["display_name"].as_str().cmp(&b["display_name"].as_str()));
    out
}

/// Health of every node of a view.
pub fn health_of(v: &View) -> BTreeMap<String, Value> {
    let reached = v.reached();
    let mut risky_starts: Vec<String> = Vec::new();
    let mut risky_tests: BTreeSet<String> = BTreeSet::new();
    for (rid, r) in &v.requirements {
        if r["at_risk"] == true {
            risky_starts.extend(v.applies.get(rid).cloned().unwrap_or_default());
            risky_tests.extend(v.verified_by.get(rid).cloned().unwrap_or_default());
        }
    }
    let mut at_risk = v.reach(&risky_starts, false);
    at_risk.extend(risky_tests);
    let mut out = BTreeMap::new();
    for n in v.tree.version_of.keys() {
        let beneath = v.reach(std::slice::from_ref(n), false);
        let mut tids = v.tested_with(&beneath);
        if is_leaf(n) {
            let k = crate::key_of(n);
            match v.g.symbols.get(k) {
                Some(s) if s.kind == Kind::Test => {
                    tids.insert(k.to_string());
                }
                Some(_) => tids.extend(v.g.tests_reaching(k)),
                None => {}
            }
        }
        let statuses: Vec<String> = tids
            .iter()
            .map(|t| {
                v.states
                    .get(t)
                    .map(|s| s.status.clone())
                    .unwrap_or_else(|| "not_run".into())
            })
            .collect();
        let failing = statuses.iter().filter(|s| *s == "fail").count();
        let pending = statuses.iter().filter(|s| *s == "not_run").count();
        let is_reached = !is_leaf(n) || reached.contains(n);
        let risky = at_risk.contains(n);
        let health = if !is_reached {
            "orphan"
        } else if failing > 0 {
            "fail"
        } else if risky || pending > 0 {
            "at_risk"
        } else if tids.is_empty() {
            "uncovered"
        } else {
            "pass"
        };
        out.insert(
            n.clone(),
            json!({"id": n, "tests": tids.len(), "failing": failing, "pending": pending,
                   "reached": is_reached, "at_risk": risky, "health": health}),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_folding() {
        assert_eq!(worst(&["pass", "at_risk"]), "at_risk");
        assert_eq!(worst(&["pass", "fail", "at_risk"]), "fail");
        assert_eq!(worst(&[]), "uncovered");
    }
}
