//! Traceability queries (doc 02): coverage, orphans, impact, duplicates, plus per-node health
//! for the map. Graph work is done in AQL; group logic (all_of / any_of) is folded in Rust.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Result, Store};

/// Edges along which "beneath" / reachability is computed.
pub const REACH_EDGES: &str = "contains, calls, depends_on, member_of";

/// Test result lookup at the test symbol's *current* closure hash.
const TEST_STATUS_FN: &str = "
    LET __t = DOCUMENT(tid)
    LET __cv = FIRST(FOR e IN @@current_version FILTER e._to == tid RETURN PARSE_IDENTIFIER(e._from).key)
    LET __run = FIRST(FOR tr IN @@test_runs FILTER tr.test_version == __cv AND tr.closure_hash == __t.closure_hash
                       SORT tr.ran_at DESC LIMIT 1 RETURN tr)
";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestStatus {
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
    pub revision: i64,
    pub at_risk: bool,
    /// pass | fail | at_risk | uncovered
    pub status: String,
    pub satisfiers: Vec<String>,
    pub tests: Vec<TestStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupCoverage {
    pub id: String,
    pub key: String,
    pub title: String,
    pub logic: String,
    pub status: String,
    pub members: Vec<String>,
    pub satisfiers: Vec<String>,
    pub tests: Vec<TestStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Coverage {
    pub requirements: Vec<RequirementCoverage>,
    pub groups: Vec<GroupCoverage>,
    pub summary: BTreeMap<String, usize>,
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

/// Combine statuses with all_of semantics (worst wins).
fn worst(statuses: &[&str]) -> &'static str {
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

fn best(statuses: &[&str]) -> &'static str {
    if statuses.contains(&"pass") {
        "pass"
    } else if statuses.contains(&"at_risk") {
        "at_risk"
    } else if statuses.contains(&"fail") {
        "fail"
    } else {
        "uncovered"
    }
}

impl Store {
    /// Coverage for every requirement (or one) and every requirement group.
    pub async fn coverage(&self, requirement: Option<&str>) -> Result<Coverage> {
        let q = format!(
            "FOR r IN @@requirements
               FILTER @only == null OR r._key == @only OR r._id == @only
               LET satisfiers = (FOR e IN @@satisfies FILTER e._to == r._id RETURN e._from)
               LET reach = UNIQUE(FOR s IN satisfiers FOR v IN 0..30 OUTBOUND s {REACH_EDGES}
                                    OPTIONS {{uniqueVertices: 'global', order: 'bfs'}} RETURN v._id)
               LET reach_tests = (FOR id IN reach FOR e IN @@tested_by FILTER e._from == id RETURN e._to)
               LET verifying = (FOR e IN @@verifies FILTER e._to == r._id RETURN e._from)
               LET tests = (FOR tid IN UNIQUE(APPEND(reach_tests, verifying))
                              {TEST_STATUS_FN}
                              SORT __t.display_name
                              RETURN {{test: tid, name: __t.display_name, language: __t.language,
                                      status: __run ? __run.status : 'not_run', ran_at: __run.ran_at,
                                      duration_ms: __run.duration_ms}})
               SORT r.title
               RETURN {{id: r._id, key: r._key, title: r.title || r.statement, statement: r.statement,
                       revision: r.revision || 1, at_risk: r.at_risk == true, satisfiers, tests}}"
        );
        let rows = self.aql(&q, json!({"only": requirement})).await?;
        let mut reqs: Vec<RequirementCoverage> = Vec::new();
        for r in rows {
            let tests: Vec<TestStatus> =
                serde_json::from_value(r["tests"].clone()).unwrap_or_default();
            let at_risk = r["at_risk"].as_bool().unwrap_or(false);
            let mut status = tests_status(&tests).to_string();
            if at_risk && status != "fail" {
                status = "at_risk".into();
            }
            reqs.push(RequirementCoverage {
                id: r["id"].as_str().unwrap_or_default().into(),
                key: r["key"].as_str().unwrap_or_default().into(),
                title: r["title"].as_str().unwrap_or_default().into(),
                statement: r["statement"].as_str().unwrap_or_default().into(),
                revision: r["revision"].as_i64().unwrap_or(1),
                at_risk,
                status,
                satisfiers: serde_json::from_value(r["satisfiers"].clone()).unwrap_or_default(),
                tests,
            });
        }
        let groups = if requirement.is_some() {
            vec![]
        } else {
            self.group_coverage(&reqs).await?
        };
        let mut summary = BTreeMap::new();
        for r in &reqs {
            *summary.entry(r.status.clone()).or_insert(0) += 1;
        }
        Ok(Coverage {
            requirements: reqs,
            groups,
            summary,
        })
    }

    async fn group_coverage(&self, reqs: &[RequirementCoverage]) -> Result<Vec<GroupCoverage>> {
        let q = format!(
            "FOR g IN @@requirement_groups
               LET members = (FOR e IN @@includes FILTER e._from == g._id RETURN e._to)
               LET satisfiers = (FOR e IN @@satisfies FILTER e._to == g._id RETURN e._from)
               LET reach = UNIQUE(FOR s IN satisfiers FOR v IN 0..30 OUTBOUND s {REACH_EDGES}
                                    OPTIONS {{uniqueVertices: 'global', order: 'bfs'}} RETURN v._id)
               LET tests = (FOR tid IN UNIQUE(FOR id IN reach FOR e IN @@tested_by FILTER e._from == id RETURN e._to)
                              {TEST_STATUS_FN}
                              RETURN {{test: tid, name: __t.display_name, language: __t.language,
                                      status: __run ? __run.status : 'not_run', ran_at: __run.ran_at,
                                      duration_ms: __run.duration_ms}})
               SORT g.title
               RETURN {{id: g._id, key: g._key, title: g.title, logic: g.logic || 'all_of', members, satisfiers, tests}}"
        );
        let rows = self.aql(&q, json!({})).await?;
        let req_status: BTreeMap<String, String> = reqs
            .iter()
            .map(|r| (r.id.clone(), r.status.clone()))
            .collect();
        let raw: BTreeMap<String, Value> = rows
            .into_iter()
            .map(|r| (r["id"].as_str().unwrap_or_default().to_string(), r))
            .collect();
        let mut memo: BTreeMap<String, String> = BTreeMap::new();
        fn resolve(
            id: &str,
            raw: &BTreeMap<String, Value>,
            req_status: &BTreeMap<String, String>,
            memo: &mut BTreeMap<String, String>,
            stack: &mut BTreeSet<String>,
        ) -> String {
            if let Some(s) = memo.get(id) {
                return s.clone();
            }
            if let Some(s) = req_status.get(id) {
                return s.clone();
            }
            let Some(g) = raw.get(id) else {
                return "uncovered".into();
            };
            if !stack.insert(id.to_string()) {
                return "uncovered".into(); // cycle guard
            }
            let members: Vec<String> =
                serde_json::from_value(g["members"].clone()).unwrap_or_default();
            let member_statuses: Vec<String> = members
                .iter()
                .map(|m| resolve(m, raw, req_status, memo, stack))
                .collect();
            let refs: Vec<&str> = member_statuses.iter().map(String::as_str).collect();
            let combined = if g["logic"] == "any_of" {
                best(&refs)
            } else {
                worst(&refs)
            };
            let tests: Vec<TestStatus> =
                serde_json::from_value(g["tests"].clone()).unwrap_or_default();
            let own = tests_status(&tests);
            let status = if tests.is_empty() {
                combined
            } else {
                worst(&[combined, own])
            };
            stack.remove(id);
            memo.insert(id.to_string(), status.to_string());
            status.to_string()
        }
        let mut out = Vec::new();
        for (id, g) in &raw {
            let status = resolve(id, &raw, &req_status, &mut memo, &mut BTreeSet::new());
            out.push(GroupCoverage {
                id: id.clone(),
                key: g["key"].as_str().unwrap_or_default().into(),
                title: g["title"].as_str().unwrap_or_default().into(),
                logic: g["logic"].as_str().unwrap_or("all_of").into(),
                status,
                members: serde_json::from_value(g["members"].clone()).unwrap_or_default(),
                satisfiers: serde_json::from_value(g["satisfiers"].clone()).unwrap_or_default(),
                tests: serde_json::from_value(g["tests"].clone()).unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Symbols not reachable from any node with a `satisfies` edge (tests that verify a
    /// requirement count as reachable roots).
    pub async fn orphans(&self) -> Result<Vec<Value>> {
        self.orphans_in(None).await
    }

    pub async fn orphans_in(&self, trx: Option<&str>) -> Result<Vec<Value>> {
        self.db.aql(
            &format!(
                "LET starts = UNIQUE(APPEND((FOR e IN @@satisfies RETURN e._from), (FOR e IN @@verifies RETURN e._from)))
                 LET reached = UNIQUE(FOR s IN starts FOR v IN 0..30 OUTBOUND s {REACH_EDGES}, tested_by, binds_to
                                        OPTIONS {{uniqueVertices: 'global', order: 'bfs'}} RETURN v._id)
                 FOR s IN @@symbols FILTER s._id NOT IN reached SORT s.display_name
                 RETURN {{id: s._id, key: s._key, display_name: s.display_name, kind: s.kind, language: s.language}}"
            ),
            json!({}),
            trx,
        )
        .await
    }

    /// Everything at risk if `symbol` changes: dependants upward, tests that exercise it, and the
    /// requirements / groups those reach.
    pub async fn impact(&self, symbol: &str) -> Result<Value> {
        let sid = if symbol.contains('/') {
            symbol.to_string()
        } else {
            format!("symbols/{symbol}")
        };
        self.get_node(&sid).await?;
        let rows = self
            .aql(
                "LET up = UNIQUE(FOR v IN 0..30 INBOUND @s @@calls, @@depends_on, @@contains, @@member_of, @@binds_to
                                   OPTIONS {uniqueVertices: 'global', order: 'bfs'} RETURN v._id)
                 LET direct = UNIQUE(FOR id IN up FOR e IN @@satisfies FILTER e._from == id RETURN e._to)
                 LET via_tests = UNIQUE(FOR id IN up FOR e IN @@verifies FILTER e._from == id RETURN e._to)
                 LET reqs0 = UNIQUE(APPEND(direct, via_tests))
                 LET groups_up = UNIQUE(FOR r IN reqs0 FOR v IN 0..10 INBOUND r @@includes
                                          OPTIONS {uniqueVertices: 'global', order: 'bfs'} RETURN v._id)
                 LET hits = UNIQUE(APPEND(reqs0, groups_up))
                 RETURN {
                   symbols: (FOR id IN up FILTER STARTS_WITH(id, CONCAT(@pfx, 'symbols/')) LET d = DOCUMENT(id)
                             RETURN {id, display_name: d.display_name, kind: d.kind, language: d.language}),
                   nodes: (FOR id IN up FILTER !STARTS_WITH(id, CONCAT(@pfx, 'symbols/')) LET d = DOCUMENT(id)
                             RETURN {id, title: d.title || d.name}),
                   requirements: (FOR id IN hits FILTER STARTS_WITH(id, CONCAT(@pfx, 'requirements/')) LET d = DOCUMENT(id)
                                  SORT d.title RETURN {id, title: d.title, statement: d.statement}),
                   groups: (FOR id IN hits FILTER STARTS_WITH(id, CONCAT(@pfx, 'requirement_groups/')) LET d = DOCUMENT(id)
                            SORT d.title RETURN {id, title: d.title, logic: d.logic})
                 }",
                json!({"s": sid}),
            )
            .await?;
        let mut v = rows.into_iter().next().unwrap_or(json!({}));
        v["symbol"] = json!(sid);
        Ok(v)
    }

    /// Exact duplicates: symbols whose current versions share a content hash.
    pub async fn duplicates(&self) -> Result<Vec<Value>> {
        self.aql(
            "FOR e IN @@current_version COLLECT v = e._from INTO g
               FILTER LENGTH(g) > 1
               LET syms = (FOR x IN g LET s = DOCUMENT(x.e._to) SORT s.display_name
                           RETURN {id: s._id, display_name: s.display_name, language: s.language})
               SORT v
               RETURN {content_hash: PARSE_IDENTIFIER(v).key, symbols: syms}",
            json!({}),
        )
        .await
    }

    /// Near duplicates (doc 02, M5+): symbol pairs of the same language and kind whose current
    /// bodies have Jaccard similarity ≥ `threshold` over token 3-grams (exact duplicates excluded).
    /// A deterministic stand-in for embeddings (decisions.md D-035).
    pub async fn near_duplicates(&self, threshold: f64) -> Result<Vec<Value>> {
        let g = self.load_code_graph().await?;
        let syms: Vec<_> = g
            .symbols
            .values()
            .filter(|s| s.kind != kodehub_core::model::Kind::Test)
            .collect();
        let sh: Vec<_> = syms
            .iter()
            .map(|s| {
                let v = g.version(&s.version).ok();
                v.map(|v| {
                    kodehub_core::infer::shingles(
                        s.language,
                        &format!("{}\n{}", v.signature, v.body),
                    )
                })
                .unwrap_or_default()
            })
            .collect();
        let mut out = Vec::new();
        for i in 0..syms.len() {
            for j in i + 1..syms.len() {
                let (a, b) = (syms[i], syms[j]);
                if a.language != b.language || a.kind != b.kind || a.version == b.version {
                    continue;
                }
                let score = kodehub_core::infer::similarity(&sh[i], &sh[j]);
                if score >= threshold {
                    out.push(json!({"a": {"id": format!("symbols/{}", a.id), "display_name": a.display_name},
                                    "b": {"id": format!("symbols/{}", b.id), "display_name": b.display_name},
                                    "similarity": (score * 1000.0).round() / 1000.0}));
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

    /// Health of every map node (C4 nodes and symbols) for colouring:
    /// fail > at_risk > pass; uncovered (no tests beneath); orphan (no requirement reaches it).
    pub async fn node_health(&self) -> Result<BTreeMap<String, Value>> {
        let q = format!(
            "LET starts = UNIQUE(APPEND((FOR e IN @@satisfies RETURN e._from), (FOR e IN @@verifies RETURN e._from)))
             LET reached = UNIQUE(FOR s IN starts FOR v IN 0..30 OUTBOUND s {REACH_EDGES}, tested_by, binds_to
                                    OPTIONS {{uniqueVertices: 'global', order: 'bfs'}} RETURN v._id)
             LET risky_roots = UNIQUE(FOR r IN @@requirements FILTER r.at_risk == true
                                      FOR e IN @@satisfies FILTER e._to == r._id RETURN e._from)
             LET risky_tests = UNIQUE(FOR r IN @@requirements FILTER r.at_risk == true
                                      FOR e IN @@verifies FILTER e._to == r._id RETURN e._from)
             LET at_risk = UNIQUE(APPEND(risky_tests, (FOR s IN risky_roots FOR v IN 0..30 OUTBOUND s {REACH_EDGES}
                                    OPTIONS {{uniqueVertices: 'global', order: 'bfs'}} RETURN v._id)))
             FOR n IN UNION(
                 (FOR x IN @@contexts RETURN x), (FOR x IN @@containers RETURN x),
                 (FOR x IN @@components RETURN x), (FOR x IN @@symbols RETURN x))
               LET beneath = UNIQUE(FOR v IN 0..30 OUTBOUND n {REACH_EDGES}
                                      OPTIONS {{uniqueVertices: 'global', order: 'bfs'}} RETURN v._id)
               // symbols: also every test whose closure reaches this symbol (helpers and types are
               // covered by the tests that exercise them, decisions.md D-031)
               LET reaching = STARTS_WITH(n._id, CONCAT(@pfx, 'symbols/')) ? (
                   FOR v IN 1..30 INBOUND n @@calls, @@depends_on, @@member_of, @@binds_to
                     OPTIONS {{uniqueVertices: 'global', order: 'bfs'}}
                     FILTER v.kind == 'test' RETURN v._id) : []
               LET tids = UNIQUE(UNION(
                   (FOR id IN beneath FOR e IN @@tested_by FILTER e._from == id RETURN e._to),
                   reaching,
                   n.kind == 'test' ? [n._id] : []))
               LET statuses = (FOR tid IN tids {TEST_STATUS_FN} RETURN __run ? __run.status : 'not_run')
               RETURN {{id: n._id, tests: LENGTH(tids),
                       failing: LENGTH(statuses[* FILTER CURRENT == 'fail']),
                       pending: LENGTH(statuses[* FILTER CURRENT == 'not_run']),
                       reached: n._id IN reached OR !STARTS_WITH(n._id, CONCAT(@pfx, 'symbols/')),
                       at_risk: n._id IN at_risk}}"
        );
        let rows = self.aql(&q, json!({})).await?;
        let mut out = BTreeMap::new();
        for r in rows {
            let id = r["id"].as_str().unwrap_or_default().to_string();
            let tests = r["tests"].as_i64().unwrap_or(0);
            let health = if !r["reached"].as_bool().unwrap_or(true) {
                "orphan"
            } else if r["failing"].as_i64().unwrap_or(0) > 0 {
                "fail"
            } else if r["at_risk"].as_bool().unwrap_or(false)
                || r["pending"].as_i64().unwrap_or(0) > 0
            {
                "at_risk"
            } else if tests == 0 {
                "uncovered"
            } else {
                "pass"
            };
            let mut v = r.clone();
            v["health"] = json!(health);
            out.insert(id, v);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_folding() {
        assert_eq!(worst(&["pass", "at_risk"]), "at_risk");
        assert_eq!(worst(&["pass", "fail", "at_risk"]), "fail");
        assert_eq!(best(&["fail", "pass"]), "pass");
        assert_eq!(worst(&[]), "uncovered");
    }
}
