//! Work items carry the proposal (D-058, docs/ontology.md "Work without changesets").
//!
//! A code-changing work item holds its proposal: plan, candidate versions, edges, review
//! findings, test results, activity and its stage (draft → planned → approved → building →
//! reviewing → testing → ready → accepted | rejected | escalated). Proposed versions are real,
//! immutable version nodes with a `has_version` edge in state `candidate` tagged with the work
//! item, so no project version sees them. **Accepting** writes every new node and edge and
//! moves the work item's open project version to the new versions in one transaction:
//! all or nothing, with the ancestor chain built against the tree as it is at that moment.
//!
//! The proposal is the work item's `proposal` field; [`Store::get_proposal`] returns it with
//! `_key` = the work item key (the id agents and the API use), `work_item`, `project` and
//! `project_version`.

use std::collections::{BTreeMap, BTreeSet};

use kodehub_core::infer::references;
use kodehub_core::model::{
    now_iso, CodeEdgeKind, Kind, Language, ProposalState, VersionContent, Visibility,
};
use kodehub_core::{CodeEdge, CodeGraph, CodeSymbol};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::queue::WORK_ITEMS;
use crate::store::{edge_key, key_of, version_content_from_doc};
use crate::tree::TreeChanges;
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProposeVersion {
    /// Existing code or test key; omit to propose a new node.
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub kind: Option<Kind>,
    #[serde(default)]
    pub language: Option<Language>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    pub signature: String,
    pub body: String,
    #[serde(default)]
    pub attributes: Option<Value>,
    /// Component key that should hold a new node.
    #[serde(default)]
    pub component: Option<String>,
    /// Class key for a new method.
    #[serde(default)]
    pub member_of: Option<String>,
    /// Requirement keys a test verifies (`verified_by` edges, requirement → test).
    #[serde(default)]
    pub verifies: Vec<String>,
    /// Code keys a test exercises (`tested_with` edges, code → test).
    #[serde(default)]
    pub tests_for: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AcceptChecks {
    pub tests_ok: bool,
    pub review_ok: bool,
    /// The project's `blind_review` setting at check time.
    #[serde(default)]
    pub review_required: bool,
    /// Requirement edits: each requirement has a proposed test verifying it (review M6 #1).
    #[serde(default)]
    pub requirements_covered: bool,
    #[serde(default)]
    pub uncovered_requirements: Vec<String>,
    #[serde(default)]
    pub review_attested: bool,
    pub orphans_ok: Option<bool>,
    pub affected_tests: Vec<String>,
    pub failing_tests: Vec<String>,
    pub missing_tests: Vec<String>,
    pub open_blocking: Vec<Value>,
    pub new_orphans: Vec<String>,
    /// Tests failing here that already failed at the baseline (not blamed on the change).
    #[serde(default)]
    pub preexisting_failures: Vec<String>,
}

impl AcceptChecks {
    pub fn ok(&self) -> bool {
        self.tests_ok
            && self.review_ok
            && self.requirements_covered
            && self.orphans_ok.unwrap_or(true)
    }
}

/// Key a new code node will get on acceptance.
pub fn new_symbol_key(language: Language, display_name: &str) -> String {
    match language {
        Language::Rust => display_name.replace("::", "-"),
        Language::Python => format!("py-{}", display_name.replace('.', "-")),
    }
}

/// Collections written when a proposal is accepted.
const ACCEPT_WRITE: &[&str] = &[
    "code",
    "tests",
    "code_versions",
    "test_versions",
    "project_versions",
    "context_versions",
    "container_versions",
    "component_versions",
    "has_version",
    "contains",
    "calls",
    "depends_on",
    "member_of",
    "tested_with",
    "binds_to",
    "verified_by",
    "applies_to",
    "requirements",
    WORK_ITEMS,
];

fn node_coll(kind: &Value) -> &'static str {
    if kind == "test" {
        "tests"
    } else {
        "code"
    }
}

impl Store {
    // ------------------------------------------------------------------ the proposal document

    /// Attach a proposal to a work item. `requirements` are the requirement keys it
    /// addresses; their current revisions are recorded.
    pub async fn open_proposal(
        &self,
        work_item: &str,
        title: &str,
        intent: &str,
        requirements: &[String],
        created_by: &str,
    ) -> Result<Value> {
        let w = self.work_item(work_item).await?;
        if w["proposal"].is_object() {
            return self.get_proposal(work_item).await;
        }
        let now = now_iso();
        let reqs: Vec<String> = requirements
            .iter()
            .map(|r| self.db.pkey(key_of(r)))
            .collect::<Result<_>>()?;
        let mut addresses = serde_json::Map::new();
        for r in &reqs {
            let d = self.db.get("requirements", r, None).await?;
            addresses.insert(r.clone(), d["revision"].clone());
        }
        let proposal = json!({
            "addresses": addresses,
            "title": title,
            "intent": intent,
            "state": "draft",
            "requirements": reqs,
            "plan": null,
            "proposals": {},
            "edges": [],
            "reviews": [],
            "test_results": [],
            "rounds": 0,
            "decisions": [],
            "transcripts": [],
            "activity": [],
            "history": [{"state": "draft", "at": now, "by": created_by, "note": "opened"}],
            "created_by": created_by,
            "created_at": now,
            "updated_at": now,
        });
        self.g_patch(
            WORK_ITEMS,
            key_of(work_item),
            &json!({"proposal": proposal}),
        )
        .await?;
        self.get_proposal(work_item).await
    }

    /// The proposal of a work item, with `_key` = the work item key.
    pub async fn get_proposal(&self, work_item: &str) -> Result<Value> {
        let w = self.work_item(key_of(work_item)).await?;
        let mut p = w["proposal"].clone();
        if !p.is_object() {
            return Err(StoreError::NotFound(format!(
                "work item {work_item} has no proposal"
            )));
        }
        if let Some(project) = self.project_id() {
            if w["project"] != json!(project) {
                return Err(StoreError::NotFound(format!("work item {work_item}")));
            }
        }
        p["_key"] = w["_key"].clone();
        p["_id"] = w["_id"].clone();
        p["work_item"] = w["_key"].clone();
        p["project"] = w["project"].clone();
        p["project_version"] = w["project_version"].clone();
        p["agent_type"] = w["agent_type"].clone();
        p["kind"] = w["kind"].clone();
        p["target"] = w["target"].clone();
        p["work_state"] = w["state"].clone();
        Ok(p)
    }

    /// Proposals of the active project's work items (newest first); `open_only` leaves out
    /// accepted and rejected ones.
    pub async fn list_proposals(&self, open_only: bool) -> Result<Vec<Value>> {
        let project = self.db.require_project()?;
        let rows = self
            .g_aql(
                "FOR w IN @@w FILTER w.project == @p AND w.proposal != null
                   FILTER !@open OR w.proposal.state NOT IN ['accepted', 'rejected']
                   SORT w.created_at DESC
                   RETURN MERGE(UNSET(w.proposal, 'transcripts'),
                                {_key: w._key, _id: w._id, work_item: w._key, project: w.project,
                                 project_version: w.project_version, agent_type: w.agent_type,
                                 kind: w.kind, target: w.target, work_state: w.state})",
                json!({"@w": WORK_ITEMS, "p": project, "open": open_only}),
            )
            .await?;
        Ok(rows)
    }

    /// Patch top-level fields of a proposal (each field is replaced, not merged).
    pub async fn patch_proposal(&self, work_item: &str, patch: Value) -> Result<Value> {
        let mut p = patch;
        p["updated_at"] = json!(now_iso());
        self.patch_proposal_in(work_item, &p, None).await?;
        self.get_proposal(work_item).await
    }

    async fn patch_proposal_in(
        &self,
        work_item: &str,
        patch: &Value,
        trx: Option<&str>,
    ) -> Result<()> {
        let q = "FOR w IN @@w FILTER w._key == @k
                   UPDATE w WITH {proposal: MERGE(w.proposal, @p)} IN @@w OPTIONS {mergeObjects: false}";
        let bind = json!({"@w": WORK_ITEMS, "k": key_of(work_item), "p": patch});
        match trx {
            Some(t) => self.db.aql(q, bind, Some(t)).await.map(|_| ()),
            None => self.g_aql(q, bind).await.map(|_| ()),
        }
    }

    fn state_of(p: &Value) -> Result<ProposalState> {
        serde_json::from_value(p["state"].clone())
            .map_err(|_| StoreError::Invalid(format!("bad proposal state {}", p["state"])))
    }

    /// Validated stage transition with history.
    pub async fn transition(
        &self,
        work_item: &str,
        to: ProposalState,
        by: &str,
        note: &str,
    ) -> Result<Value> {
        let p = self.get_proposal(work_item).await?;
        let from = Self::state_of(&p)?;
        if !from.can_transition_to(to) {
            return Err(StoreError::Invalid(format!(
                "work item {work_item}: cannot go from {} to {}",
                from.as_str(),
                to.as_str()
            )));
        }
        let mut history = p["history"].as_array().cloned().unwrap_or_default();
        history.push(json!({"state": to.as_str(), "at": now_iso(), "by": by, "note": note}));
        let mut patch = json!({"state": to.as_str(), "history": history});
        if to == ProposalState::Building
            && matches!(
                from,
                ProposalState::Reviewing | ProposalState::Testing | ProposalState::Ready
            )
        {
            patch["rounds"] = json!(p["rounds"].as_i64().unwrap_or(0) + 1);
        }
        self.patch_proposal(work_item, patch).await
    }

    pub async fn set_plan(&self, work_item: &str, plan: Value, by: &str) -> Result<Value> {
        // the plan addresses the requirements' revisions as they are now
        let p = self.get_proposal(work_item).await?;
        let mut addresses = serde_json::Map::new();
        for r in p["requirements"].as_array().cloned().unwrap_or_default() {
            if let Some(r) = r.as_str() {
                let d = self.db.get("requirements", r, None).await?;
                addresses.insert(r.to_string(), d["revision"].clone());
            }
        }
        self.patch_proposal(work_item, json!({"plan": plan, "addresses": addresses}))
            .await?;
        self.transition(work_item, ProposalState::Planned, by, "plan attached")
            .await
    }

    /// Append an activity entry (agent log shown in the UI) and optionally a transcript entry.
    pub async fn log_activity(
        &self,
        work_item: &str,
        entry: Value,
        transcript: Option<Value>,
    ) -> Result<()> {
        let p = self.get_proposal(work_item).await?;
        let mut act = p["activity"].as_array().cloned().unwrap_or_default();
        let mut e = entry;
        e["at"] = json!(now_iso());
        act.push(e);
        let mut patch = json!({"activity": act});
        if let Some(t) = transcript {
            let mut tr = p["transcripts"].as_array().cloned().unwrap_or_default();
            tr.push(t);
            patch["transcripts"] = json!(tr);
        }
        self.patch_proposal(work_item, patch).await.map(|_| ())
    }

    fn ensure_editable(p: &Value) -> Result<()> {
        let st = Self::state_of(p)?;
        if st.is_terminal() {
            return Err(StoreError::Invalid(format!(
                "the proposal is {}; no further changes",
                st.as_str()
            )));
        }
        Ok(())
    }

    /// The open project version a work item's changes go into.
    async fn work_item_version(&self, p: &Value) -> Result<String> {
        match p["project_version"].as_str() {
            Some(v) => Ok(v.to_string()),
            None => self.current_version().await,
        }
    }

    // ------------------------------------------------------------------ proposing

    /// Propose a version (of an existing or a new code or test node) for a work item. The
    /// version is written as a candidate; nothing changes in any project version.
    pub async fn propose_version(&self, work_item: &str, pv: &ProposeVersion) -> Result<Value> {
        let p = self.get_proposal(work_item).await?;
        Self::ensure_editable(&p)?;
        let wi = key_of(work_item).to_string();
        let (pkey, kind, language, display_name, is_new) = match &pv.symbol {
            Some(s) => {
                let id = self.symbol_id(s).await?;
                let (coll, k) = id.split_once('/').unwrap_or_default();
                let sym = self.db.get(coll, k, None).await?;
                let kind: Kind = serde_json::from_value(sym["kind"].clone())
                    .map_err(|e| StoreError::Invalid(format!("kind: {e}")))?;
                let lang: Language = serde_json::from_value(sym["language"].clone())
                    .map_err(|e| StoreError::Invalid(format!("language: {e}")))?;
                (
                    k.to_string(),
                    kind,
                    lang,
                    sym["display_name"].as_str().unwrap_or_default().to_string(),
                    false,
                )
            }
            None => {
                let name = pv
                    .display_name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .ok_or_else(|| StoreError::Invalid("new code needs display_name".into()))?;
                let kind = pv
                    .kind
                    .ok_or_else(|| StoreError::Invalid("new code needs kind".into()))?;
                let lang = pv
                    .language
                    .ok_or_else(|| StoreError::Invalid("new code needs language".into()))?;
                let sk = self.db.pkey(&new_symbol_key(lang, &name))?;
                // a node with that name may already exist → treat as modification
                let existing = self
                    .aql(
                        "FOR s IN UNION((FOR x IN @@code RETURN x), (FOR x IN @@tests RETURN x))
                           FILTER s.language == @l AND s.display_name == @n RETURN s",
                        json!({"l": lang, "n": name}),
                    )
                    .await?;
                if let Some(doc) = existing.first() {
                    if doc["kind"] != json!(kind) {
                        return Err(StoreError::Invalid(format!(
                            "`{name}` already exists as a {}; a {} can't replace it",
                            doc["kind"].as_str().unwrap_or("?"),
                            kind.as_str()
                        )));
                    }
                    let k = doc["_key"].as_str().unwrap_or_default().to_string();
                    (k, kind, lang, name, false)
                } else {
                    (sk, kind, lang, name, true)
                }
            }
        };
        let coll = Self::node_collection(kind);
        let node_id = format!("{coll}/{pkey}");
        let by = pv.by.clone().unwrap_or_else(|| "agent".into());
        let pv_id = self.work_item_version(&p).await?;
        let base_version: Option<String> = if is_new {
            None
        } else {
            self.tree(Some(&pv_id))
                .await?
                .version(&node_id)
                .map(str::to_string)
        };
        // attributes left out keep the base version's (imports, derives, …)
        let attributes = match (&pv.attributes, &base_version) {
            (Some(a), _) => a.clone(),
            (None, Some(b)) => self.get_node(b).await?["attributes"].clone(),
            (None, None) => json!({}),
        };
        let content = VersionContent {
            kind,
            language,
            signature: pv.signature.clone(),
            body: pv.body.clone(),
            attributes: if attributes.is_object() {
                attributes
            } else {
                json!({})
            },
        };
        let n = content.normalized();
        // the version is written now; a new node only exists once the proposal is accepted,
        // so its version document waits outside the graph's edges until then
        let (vid, doc) = self.leaf_version_doc(&node_id, &content, &by)?;
        let vcoll = vid.split_once('/').map(|x| x.0).unwrap_or_default();
        match self.db.insert(vcoll, &doc, None).await {
            Ok(_) => {}
            Err(e) if e.is_unique_violation() => {}
            Err(e) => return Err(e),
        }
        if !is_new {
            self.put_leaf_version(&node_id, &content, &by, Some(&wi), None)
                .await?;
        }
        let proposal = json!({
            "base_version": base_version,
            "symbol": pkey,
            "node": node_id,
            "new": is_new,
            "display_name": display_name,
            "kind": kind,
            "language": language,
            "visibility": pv.visibility.unwrap_or_default(),
            "version": key_of(&vid),
            "version_id": vid,
            "signature": n.signature,
            "body": n.body,
            "attributes": n.attributes,
            "component": pv.component.as_deref().map(|c| self.db.pkey(key_of(c))).transpose()?,
            "member_of": pv.member_of.as_deref().map(|c| self.db.pkey(key_of(c))).transpose()?,
            "verifies": pv.verifies.iter().map(|r| self.db.pkey(key_of(r))).collect::<Result<Vec<_>>>()?,
            "tests_for": pv.tests_for.iter().map(|r| self.db.pkey(key_of(r))).collect::<Result<Vec<_>>>()?,
            "description": pv.description,
            "by": by,
            "round": p["rounds"],
            "at": now_iso(),
        });
        let mut proposals = p["proposals"].as_object().cloned().unwrap_or_default();
        proposals.insert(pkey.clone(), proposal.clone());
        self.patch_proposal(work_item, json!({"proposals": proposals}))
            .await?;
        Ok(proposal)
    }

    /// Withdraw a proposed version.
    pub async fn withdraw_proposal(&self, work_item: &str, symbol: &str) -> Result<()> {
        let p = self.get_proposal(work_item).await?;
        Self::ensure_editable(&p)?;
        let mut proposals = p["proposals"].as_object().cloned().unwrap_or_default();
        let symbol = self.db.pkey(key_of(symbol))?;
        if let Some(old) = proposals.remove(&symbol) {
            if let Some(n) = old["node"].as_str() {
                self.discard_candidates(key_of(work_item), Some(n)).await?;
            }
        }
        self.patch_proposal(work_item, json!({"proposals": proposals}))
            .await
            .map(|_| ())
    }

    /// Propose an edge (code edges, `applies_to`, `verified_by`, …) for a work item.
    pub async fn propose_edge(
        &self,
        work_item: &str,
        collection: &str,
        from: &str,
        to: &str,
        data: Value,
    ) -> Result<Value> {
        let p = self.get_proposal(work_item).await?;
        Self::ensure_editable(&p)?;
        crate::store::check_edge_collection(collection)?;
        let (from, to) = (&self.db.qid(from)?, &self.db.qid(to)?);
        let mut edges = p["edges"].as_array().cloned().unwrap_or_default();
        let e = json!({"collection": collection, "from": from, "to": to, "data": data});
        if !edges.contains(&e) {
            edges.push(e.clone());
        }
        self.patch_proposal(work_item, json!({"edges": edges}))
            .await?;
        Ok(e)
    }

    /// Store a review round. Findings get ids and `status: open`.
    pub async fn submit_review(
        &self,
        work_item: &str,
        findings: &[Value],
        summary: &str,
        by: &str,
        source: &str,
    ) -> Result<Value> {
        let p = self.get_proposal(work_item).await?;
        Self::ensure_editable(&p)?;
        let mut reviews = p["reviews"].as_array().cloned().unwrap_or_default();
        let round = reviews.len();
        let fs: Vec<Value> = findings
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let sev = f["severity"].as_str().unwrap_or("minor").to_lowercase();
                let sev = if ["blocking", "major", "minor", "info"].contains(&sev.as_str()) {
                    sev
                } else {
                    "minor".into()
                };
                json!({
                    "id": f["id"].as_str().filter(|x| !x.is_empty()).map(String::from).unwrap_or_else(|| format!("r{round}-f{i}")),
                    "severity": sev,
                    "location": f["location"].as_str().unwrap_or(""),
                    "finding": f["finding"].as_str().unwrap_or(""),
                    "status": f["status"].as_str().unwrap_or("open"),
                    "resolution": f.get("resolution").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        // `source` is set by the server, never taken from a request body: only the engine's blind
        // reviewer or a human in the UI can produce a review that satisfies hard rule 6.
        let review = json!({"round": round, "findings": fs, "summary": summary, "by": by, "at": now_iso(),
                            "source": source, "fingerprint": proposals_fingerprint(&p)});
        reviews.push(review.clone());
        self.patch_proposal(work_item, json!({"reviews": reviews}))
            .await?;
        Ok(review)
    }

    /// Update the status/resolution of findings in the reviews.
    pub async fn resolve_findings(
        &self,
        work_item: &str,
        updates: &BTreeMap<String, (String, String)>,
    ) -> Result<()> {
        let p = self.get_proposal(work_item).await?;
        let mut reviews = p["reviews"].as_array().cloned().unwrap_or_default();
        for r in reviews.iter_mut() {
            if let Some(fs) = r["findings"].as_array_mut() {
                for f in fs.iter_mut() {
                    if let Some((status, resolution)) =
                        f["id"].as_str().and_then(|id| updates.get(id))
                    {
                        f["status"] = json!(status);
                        f["resolution"] = json!(resolution);
                    }
                }
            }
        }
        self.patch_proposal(work_item, json!({"reviews": reviews}))
            .await
            .map(|_| ())
    }

    // ------------------------------------------------------------------ checking

    /// The work item's project version graph with its proposals and edges applied.
    pub async fn overlay_graph(&self, work_item: &str) -> Result<(CodeGraph, Vec<String>)> {
        let p = self.get_proposal(work_item).await?;
        let pv = self.work_item_version(&p).await?;
        let mut g = self.load_code_graph_at(Some(&pv)).await?;
        let proposals = p["proposals"].as_object().cloned().unwrap_or_default();
        let keys: Vec<String> = proposals
            .values()
            .filter_map(|x| x["version"].as_str().map(String::from))
            .collect();
        let versions = self.load_versions(&keys).await?;
        let mut touched = Vec::new();
        for (pkey, x) in &proposals {
            let vkey = x["version"].as_str().unwrap_or_default().to_string();
            let content = versions
                .get(&vkey)
                .cloned()
                .ok_or_else(|| StoreError::NotFound(format!("proposed version {vkey}")))?;
            match g.symbols.get_mut(pkey) {
                Some(s) => s.version = vkey.clone(),
                None => {
                    g.symbols.insert(
                        pkey.clone(),
                        CodeSymbol {
                            id: pkey.clone(),
                            display_name: x["display_name"].as_str().unwrap_or_default().into(),
                            kind: content.kind,
                            language: content.language,
                            visibility: serde_json::from_value(x["visibility"].clone())
                                .unwrap_or_default(),
                            tags: vec![],
                            version: vkey.clone(),
                        },
                    );
                }
            }
            g.versions.insert(vkey, content);
            touched.push(pkey.clone());
        }
        // edges: member_of first (affects method name resolution), then inferred references
        for (pkey, x) in &proposals {
            if let Some(class) = x["member_of"].as_str() {
                g.edges.push(CodeEdge {
                    from: pkey.clone(),
                    to: key_of(class).into(),
                    kind: CodeEdgeKind::MemberOf,
                    pinned_version: None,
                });
            }
        }
        for (pkey, x) in &proposals {
            let lang: Language =
                serde_json::from_value(x["language"].clone()).unwrap_or(Language::Rust);
            let text = format!(
                "{}\n{}",
                x["signature"].as_str().unwrap_or(""),
                x["body"].as_str().unwrap_or("")
            );
            let refs = references(&g, pkey, lang, &text);
            // keep pins of edges that survive the rewrite (review M6 #10)
            let pins: BTreeMap<(String, CodeEdgeKind), String> = g
                .edges
                .iter()
                .filter(|e| e.from == *pkey)
                .filter_map(|e| {
                    e.pinned_version
                        .clone()
                        .map(|p| ((e.to.clone(), e.kind), p))
                })
                .collect();
            g.edges.retain(|e| {
                !(e.from == *pkey
                    && matches!(e.kind, CodeEdgeKind::Calls | CodeEdgeKind::DependsOn))
            });
            for (to, kind) in &refs {
                g.edges.push(CodeEdge {
                    from: pkey.clone(),
                    to: to.clone(),
                    kind: *kind,
                    pinned_version: pins.get(&(to.clone(), *kind)).cloned(),
                });
            }
            if x["kind"] == json!("test") {
                let mut targets: Vec<String> = x["tests_for"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                if targets.is_empty() {
                    targets = refs
                        .iter()
                        .filter(|(id, k)| *k == CodeEdgeKind::Calls && g.class_of(id).is_none())
                        .map(|(id, _)| id.clone())
                        .collect();
                }
                for t in targets {
                    if g.symbols.contains_key(&t) {
                        g.edges.push(CodeEdge {
                            from: t,
                            to: pkey.clone(),
                            kind: CodeEdgeKind::TestedWith,
                            pinned_version: None,
                        });
                    }
                }
            }
        }
        for e in p["edges"].as_array().cloned().unwrap_or_default() {
            if let Some(kind) =
                CodeEdgeKind::from_collection(e["collection"].as_str().unwrap_or(""))
            {
                let (f, t) = (
                    e["from"].as_str().unwrap_or(""),
                    e["to"].as_str().unwrap_or(""),
                );
                let leaf = |x: &str| x.starts_with("code/") || x.starts_with("tests/");
                if leaf(f) && leaf(t) {
                    g.edges.push(CodeEdge {
                        from: key_of(f).into(),
                        to: key_of(t).into(),
                        kind,
                        pinned_version: None,
                    });
                }
            }
        }
        g.edges.sort();
        g.edges.dedup();
        Ok((g, touched))
    }

    /// Tests that must pass for this proposal: tests reaching any touched node, tests that
    /// verify its requirements, and new tests.
    pub async fn affected_tests(
        &self,
        work_item: &str,
        g: &CodeGraph,
        touched: &[String],
    ) -> Result<Vec<String>> {
        let p = self.get_proposal(work_item).await?;
        let mut out: BTreeSet<String> = BTreeSet::new();
        for t in touched {
            out.extend(g.tests_reaching(t));
        }
        let reqs: Vec<String> = p["requirements"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|r| format!("requirements/{r}"))
                    .collect()
            })
            .unwrap_or_default();
        let verifying = self
            .aql(
                "FOR e IN @@verified_by FILTER e._from IN @r RETURN PARSE_IDENTIFIER(e._to).key",
                json!({"r": reqs}),
            )
            .await?;
        out.extend(verifying.iter().filter_map(Value::as_str).map(String::from));
        for (pk, x) in p["proposals"].as_object().cloned().unwrap_or_default() {
            if x["kind"] == json!("test") {
                out.insert(pk);
            }
        }
        Ok(out
            .into_iter()
            .filter(|t| g.symbols.contains_key(t))
            .collect())
    }

    /// The latest formal status of a test version at a closure hash.
    async fn formal_status(&self, version: &str, closure: &str) -> Result<Option<String>> {
        Ok(self
            .aql(
                "FOR r IN @@test_logs FILTER r.test_version == @v AND r.closure_hash == @c
                   AND r.formal != false AND r.kind != 'run'
                 SORT r.ran_at DESC LIMIT 1 RETURN r.status",
                json!({"v": version, "c": closure}),
            )
            .await?
            .first()
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// Acceptance checks (tests + review); orphans are checked inside the acceptance
    /// transaction.
    pub async fn accept_checks(
        &self,
        work_item: &str,
    ) -> Result<(AcceptChecks, CodeGraph, Vec<String>)> {
        let p = self.get_proposal(work_item).await?;
        let (g, touched) = self.overlay_graph(work_item).await?;
        let affected = self.affected_tests(work_item, &g, &touched).await?;
        let mut failing = Vec::new();
        let mut missing = Vec::new();
        let mut preexisting = Vec::new();
        // the baseline: the same test in the project version without this change (spec §5.6)
        let pv = self.work_item_version(&p).await?;
        let base = self.load_code_graph_at(Some(&pv)).await?;
        for t in &affected {
            let s = g.symbol(t)?;
            let closure = g.closure_hash(t)?;
            // only formal runs count; agents' own runs are informal
            match self.formal_status(&s.version, &closure).await?.as_deref() {
                Some("pass") => {}
                Some(_) => {
                    let was_failing = match (base.symbols.get(t), base.closure_hash(t).ok()) {
                        (Some(bs), Some(bc)) => self
                            .formal_status(&bs.version, &bc)
                            .await?
                            .is_some_and(|st| st != "pass"),
                        _ => false,
                    };
                    if was_failing {
                        // already failing before the change: the candidate is not blamed
                        preexisting.push(t.clone());
                    } else {
                        failing.push(t.clone());
                    }
                }
                None => missing.push(t.clone()),
            }
        }
        let reviews = p["reviews"].as_array().cloned().unwrap_or_default();
        // hard rule 6: the passing review must be of exactly what would be accepted
        let review_current = reviews
            .last()
            .map(|r| r["fingerprint"] == json!(proposals_fingerprint(&p)))
            .unwrap_or(false);
        // every blocking finding ever raised must end resolved (latest status per id wins)
        let mut latest: BTreeMap<String, Value> = BTreeMap::new();
        for r in &reviews {
            for f in r["findings"].as_array().cloned().unwrap_or_default() {
                latest.insert(f["id"].as_str().unwrap_or("").to_string(), f);
            }
        }
        let open_blocking: Vec<Value> = latest
            .into_values()
            .filter(|f| f["severity"] == "blocking" && f["status"] == "open")
            .collect();
        // only an attested review counts (engine reviewer or a human in the UI), and never one
        // by an author of the proposal, whatever the agent types' permissions say (D-062)
        let authors: BTreeSet<String> = p["proposals"]
            .as_object()
            .map(|o| {
                o.values()
                    .filter_map(|x| x["by"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let attested = reviews
            .last()
            .map(|r| {
                matches!(
                    r["source"].as_str(),
                    Some("engine_reviewer") | Some("human")
                ) && !authors.contains(r["by"].as_str().unwrap_or(""))
            })
            .unwrap_or(false);
        // a listed requirement whose revision is newer than its covered revision needs a
        // proposed test that verifies it (final review #7)
        let mut uncovered_reqs = Vec::new();
        for r in p["requirements"].as_array().cloned().unwrap_or_default() {
            let Some(rk) = r.as_str() else { continue };
            let d = self.db.get("requirements", rk, None).await?;
            let rev = d["revision"].as_i64().unwrap_or(1);
            let covered = d["covered_revision"].as_i64().unwrap_or(rev);
            if rev <= covered {
                continue;
            }
            let has_test = p["proposals"]
                .as_object()
                .map(|o| {
                    o.values().any(|x| {
                        x["kind"] == "test"
                            && x["verifies"]
                                .as_array()
                                .map(|v| v.contains(&r))
                                .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            if !has_test {
                uncovered_reqs.push(rk.to_string());
            }
        }
        // spec §5.6: blind review is a project setting, on by default (hard rule 6)
        let review_required = match self.project_id() {
            Some(p) => self
                .setting(&p, "blind_review")
                .await?
                .as_bool()
                .unwrap_or(true),
            None => true,
        };
        let checks = AcceptChecks {
            tests_ok: failing.is_empty() && missing.is_empty() && !affected.is_empty(),
            review_ok: !review_required
                || (!reviews.is_empty() && open_blocking.is_empty() && review_current && attested),
            review_required,
            requirements_covered: uncovered_reqs.is_empty(),
            uncovered_requirements: uncovered_reqs,
            review_attested: attested,
            orphans_ok: None,
            affected_tests: affected,
            failing_tests: failing,
            missing_tests: missing,
            open_blocking,
            new_orphans: vec![],
            preexisting_failures: preexisting,
        };
        Ok((checks, g, touched))
    }

    // ------------------------------------------------------------------ accepting

    /// Accept a proposal. With `dry_run` the transaction is aborted after the orphan check, so
    /// callers can see whether an acceptance *would* succeed.
    pub async fn accept_proposal(
        &self,
        work_item: &str,
        by: &str,
        dry_run: bool,
    ) -> Result<AcceptChecks> {
        self.accept_inner(work_item, by, dry_run, false).await
    }

    /// Tests before code (spec §14.3): a proposal of only new tests, written from the
    /// requirements for code that does not exist yet, is accepted without passing them (they
    /// fail until the code lands) and without a code review. Orphan checks still apply: every
    /// test must verify a requirement.
    pub async fn accept_tests_first(&self, work_item: &str, by: &str) -> Result<AcceptChecks> {
        let p = self.get_proposal(work_item).await?;
        let proposals = p["proposals"].as_object().cloned().unwrap_or_default();
        if p["tests_first"] != true
            || proposals.is_empty()
            || proposals.values().any(|x| x["kind"] != "test")
        {
            return Err(StoreError::Invalid(
                "only a tests-first proposal of new tests is accepted without passing them".into(),
            ));
        }
        let mut hist = p["history"].as_array().cloned().unwrap_or_default();
        hist.push(
            json!({"state": "ready", "at": now_iso(), "by": by, "note": "tests before code"}),
        );
        self.patch_proposal(work_item, json!({"state": "ready", "history": hist}))
            .await?;
        self.accept_inner(work_item, by, false, true).await
    }

    /// Accept a proposal whose code is not held to the test and review gates (import);
    /// orphan checks still apply.
    pub(crate) async fn accept_bypassing_tests(
        &self,
        work_item: &str,
        by: &str,
    ) -> Result<AcceptChecks> {
        self.accept_inner(work_item, by, false, true).await
    }

    async fn accept_inner(
        &self,
        work_item: &str,
        by: &str,
        dry_run: bool,
        bypass_gates: bool,
    ) -> Result<AcceptChecks> {
        // one acceptance/deployment at a time in this process; the tree compare-and-set
        // protects across replicas (review M6 #3)
        let _write = self.write_lock.lock().await;
        let wi = key_of(work_item).to_string();
        let p = self.get_proposal(&wi).await?;
        let state = Self::state_of(&p)?;
        if !dry_run && state != ProposalState::Ready {
            return Err(StoreError::Invalid(format!(
                "the proposal is {}, not ready",
                state.as_str()
            )));
        }
        let (mut checks, g, touched) = self.accept_checks(&wi).await?;
        if !dry_run
            && !bypass_gates
            && !(checks.tests_ok && checks.review_ok && checks.requirements_covered)
        {
            return Err(StoreError::Conflict(format!(
                "acceptance blocked: tests_ok={} (failing {:?}, not run {:?}), review_ok={} (attested {}, open blocking {}), requirements_covered={} {:?}",
                checks.tests_ok, checks.failing_tests, checks.missing_tests, checks.review_ok, checks.review_attested,
                checks.open_blocking.len(), checks.requirements_covered, checks.uncovered_requirements
            )));
        }
        let pv = self.work_item_version(&p).await?;
        let base_tree = self.tree(Some(&pv)).await?;
        let base_graph = self.code_graph_of(&base_tree, None).await?;
        let orphans_before: BTreeSet<String> = {
            let v = self
                .view_with(base_tree.clone(), base_graph.clone(), None)
                .await?;
            crate::queries::orphans_of(&v)
                .iter()
                .filter_map(|o| o["key"].as_str().map(String::from))
                .collect()
        };
        let proposals = p["proposals"].as_object().cloned().unwrap_or_default();
        // existing pins on touched nodes' edges survive (review M6 #10)
        let current_pins: BTreeMap<(String, String, CodeEdgeKind), String> = base_graph
            .edges
            .iter()
            .filter_map(|e| {
                e.pinned_version
                    .clone()
                    .map(|pin| ((e.from.clone(), e.to.clone(), e.kind), pin))
            })
            .collect();
        let trx = self.db.begin(ACCEPT_WRITE, &[]).await?;
        let res: Result<(BTreeMap<String, String>, i64)> = async {
            // re-read inside the transaction: a concurrent reject must win (review M6 #12)
            let in_trx = self
                .db
                .aql("FOR w IN @@w FILTER w._key == @k RETURN w.proposal.state",
                     json!({"@w": WORK_ITEMS, "k": wi}), Some(&trx))
                .await?;
            if !dry_run && in_trx.first() != Some(&json!("ready")) {
                return Err(StoreError::Conflict(format!(
                    "the proposal is {} — acceptance aborted",
                    in_trx.first().cloned().unwrap_or(Value::Null)
                )));
            }
            let mut accepted = BTreeMap::new();
            let mut changes = TreeChanges {
                work_item: Some(wi.clone()),
                ..Default::default()
            };
            // 1. nodes and versions (each change must still be based on the version it saw)
            for (pkey, x) in &proposals {
                let coll = node_coll(&x["kind"]);
                let node_id = format!("{coll}/{pkey}");
                let vid = x["version_id"].as_str().map(str::to_string).unwrap_or_else(|| {
                    format!("{}_versions/{}", if coll == "tests" { "test" } else { "code" },
                            x["version"].as_str().unwrap_or_default())
                });
                let exists = self.db.get(coll, pkey, Some(&trx)).await.is_ok();
                if x["new"] != json!(true) {
                    let cur = base_tree.version(&node_id).map(str::to_string);
                    let base = x["base_version"].as_str();
                    if base.is_some() && cur.as_deref() != base {
                        return Err(StoreError::Conflict(format!(
                            "`{}` changed since it was proposed; re-propose against the current version",
                            x["display_name"].as_str().unwrap_or(pkey)
                        )));
                    }
                } else if exists {
                    return Err(StoreError::Conflict(format!(
                        "`{}` was created by another change since this was proposed; re-propose against it",
                        x["display_name"].as_str().unwrap_or(pkey)
                    )));
                }
                let vdoc = self.db.get(vid.split_once('/').map(|v| v.0).unwrap_or_default(),
                                       key_of(&vid), Some(&trx)).await?;
                let content = version_content_from_doc(&vdoc)?;
                if !exists {
                    self.db.insert(coll, &json!({
                        "_key": pkey, "display_name": x["display_name"], "kind": x["kind"],
                        "language": x["language"], "visibility": x["visibility"], "tags": [],
                        "description": x["description"], "created_at": now_iso(),
                        "created_by": by, "work_item": wi,
                    }), Some(&trx)).await.map_err(|e| if e.is_unique_violation() {
                        StoreError::Conflict(format!("display name `{}` already taken", x["display_name"]))
                    } else { e })?;
                } else {
                    let sym = self.db.get(coll, pkey, Some(&trx)).await?;
                    if sym["kind"] != json!(content.kind) || sym["language"] != json!(content.language) {
                        return Err(StoreError::Invalid(format!("version kind/language must match `{pkey}`")));
                    }
                }
                self.put_leaf_version(&node_id, &content, by, Some(&wi), Some(&trx)).await?;
                changes.leaves.insert(node_id.clone(), Some(vid.clone()));
                accepted.insert(pkey.clone(), key_of(&vid).to_string());
            }
            // 2. code edges of touched nodes, from the overlay graph
            let id_of = |k: &str| -> String {
                match g.symbols.get(k).map(|s| s.kind) {
                    Some(Kind::Test) => format!("tests/{k}"),
                    _ => format!("code/{k}"),
                }
            };
            for pkey in &touched {
                let sid = id_of(pkey);
                for coll in ["calls", "depends_on"] {
                    self.db.aql("FOR e IN @@c FILTER e._from == @s REMOVE e IN @@c",
                                json!({"@c": coll, "s": sid}), Some(&trx)).await?;
                }
            }
            let touched_set: BTreeSet<&String> = touched.iter().collect();
            for e in &g.edges {
                if touched_set.contains(&e.from) || touched_set.contains(&e.to) {
                    let coll = e.kind.collection();
                    let (f, t) = (id_of(&e.from), id_of(&e.to));
                    let pin = e.pinned_version.clone().or_else(|| {
                        current_pins.get(&(e.from.clone(), e.to.clone(), e.kind)).cloned()
                    });
                    let mut doc = json!({"_key": edge_key(coll, &f, &t), "_from": f, "_to": t});
                    if let Some(pin) = pin {
                        doc["pinned_version"] = json!(pin);
                    }
                    self.db.insert_ignore(coll, &doc, Some(&trx)).await?;
                }
            }
            // 3. verified_by (requirement → test) and placement of new nodes
            for (pkey, x) in &proposals {
                let sid = id_of(pkey);
                for r in x["verifies"].as_array().cloned().unwrap_or_default() {
                    let rid = format!("requirements/{}", r.as_str().unwrap_or_default());
                    self.db.insert_ignore("verified_by", &json!({
                        "_key": edge_key("verified_by", &rid, &sid), "_from": rid, "_to": sid,
                    }), Some(&trx)).await?;
                }
                if x["new"] == json!(true) {
                    let comp = match x["component"].as_str() {
                        Some(c) if !c.is_empty() => Some(self.db.pid("components", key_of(c))?),
                        _ => {
                            // the component of the first related node
                            let related: Vec<String> = g.edges.iter()
                                .filter(|e| e.from == *pkey || e.to == *pkey)
                                .map(|e| id_of(if e.from == *pkey { &e.to } else { &e.from }))
                                .collect();
                            self.db.aql(
                                "FOR e IN @@contains FILTER e._to IN @r AND STARTS_WITH(e._from, 'components/') LIMIT 1 RETURN e._from",
                                json!({"r": related}), Some(&trx)).await?
                                .first().and_then(Value::as_str).map(String::from)
                        }
                    };
                    match comp {
                        Some(c) => {
                            self.db.insert_ignore("contains", &json!({
                                "_key": edge_key("contains", &c, &sid), "_from": c, "_to": sid,
                            }), Some(&trx)).await?;
                        }
                        None => {
                            return Err(StoreError::Invalid(format!(
                                "new `{}` names no component and relates to no placed code",
                                x["display_name"].as_str().unwrap_or(pkey)
                            )));
                        }
                    }
                }
            }
            // 4. explicit non-code edges
            for e in p["edges"].as_array().cloned().unwrap_or_default() {
                let coll = e["collection"].as_str().unwrap_or_default();
                let (f, t) = (e["from"].as_str().unwrap_or_default(), e["to"].as_str().unwrap_or_default());
                if CodeEdgeKind::from_collection(coll).is_some() {
                    self.db.insert_ignore(coll, &json!({"_key": edge_key(coll, f, t), "_from": f, "_to": t}), Some(&trx)).await?;
                    continue;
                }
                let mut d = e["data"].clone();
                if !d.is_object() { d = json!({}); }
                d["_key"] = json!(edge_key(coll, f, t));
                d["_from"] = json!(f);
                d["_to"] = json!(t);
                self.db.insert_ignore(coll, &d, Some(&trx)).await?;
            }
            // 5. the tree: one new version per affected ancestor, then the root moves
            let rev = self.apply_tree_changes_in(&pv, &changes, by, Some(base_tree.rev), &trx).await?;
            Ok((accepted, rev))
        }
        .await;
        let (accepted, rev) = match res {
            Ok(m) => m,
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                return Err(e);
            }
        };
        let after: BTreeSet<String> = {
            let t = std::sync::Arc::new(self.load_tree(&pv, rev, Some(&trx)).await?);
            let ag = self.code_graph_of(&t, Some(&trx)).await?;
            let v = self.view_with(t, ag, Some(&trx)).await?;
            crate::queries::orphans_of(&v)
                .iter()
                .filter_map(|o| o["key"].as_str().map(String::from))
                .collect()
        };
        checks.new_orphans = after.difference(&orphans_before).cloned().collect();
        checks.orphans_ok = Some(checks.new_orphans.is_empty());
        if dry_run || !checks.new_orphans.is_empty() {
            self.db.abort(&trx).await?;
            if !dry_run {
                return Err(StoreError::Conflict(format!(
                    "acceptance would introduce orphans: {:?}",
                    checks.new_orphans
                )));
            }
            return Ok(checks);
        }
        let finish: Result<()> = async {
            // acceptance covers each listed requirement up to the revision it addressed;
            // at_risk is derived from (revision > covered_revision) (final review #7)
            for r in p["requirements"].as_array().cloned().unwrap_or_default() {
                if let Some(r) = r.as_str() {
                    let doc = self.db.get("requirements", r, Some(&trx)).await?;
                    let rev = doc["revision"].as_i64().unwrap_or(1);
                    let prev = doc["covered_revision"].as_i64().unwrap_or(rev);
                    let addressed = p["addresses"][r].as_i64().unwrap_or(rev);
                    let covered = prev.max(addressed.min(rev));
                    self.db.patch("requirements", r,
                                  &json!({"covered_revision": covered, "at_risk": rev > covered}),
                                  Some(&trx)).await?;
                }
            }
            let mut history = p["history"].as_array().cloned().unwrap_or_default();
            history.push(json!({"state": "accepted", "at": now_iso(), "by": by,
                                "note": "every candidate accepted into the project version in one transaction"}));
            self.patch_proposal_in(&wi, &json!({
                "state": "accepted", "accepted_at": now_iso(), "accepted_by": by,
                "accepted_versions": accepted, "accepted_into": pv,
                "history": history, "checks": checks, "updated_at": now_iso(),
            }), Some(&trx)).await?;
            Ok(())
        }
        .await;
        match finish {
            Ok(()) => {
                self.db.commit(&trx).await?;
                self.trees.lock().unwrap().remove(&pv);
                // code tests run as soon as a version lands (spec §11.3)
                if let Some(project) = self.project_id() {
                    for sym in accepted.keys() {
                        let id = match g.symbols.get(sym).map(|s| s.kind) {
                            Some(Kind::Test) => format!("tests/{sym}"),
                            _ => format!("code/{sym}"),
                        };
                        if let Err(e) = self.trigger(&project, "version_landed", &id, by).await {
                            tracing::warn!("work item {wi}: no test trigger for {sym}: {e}");
                        }
                    }
                }
                Ok(checks)
            }
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                Err(e)
            }
        }
    }

    pub async fn reject_proposal(&self, work_item: &str, by: &str, reason: &str) -> Result<Value> {
        let v = self
            .transition(work_item, ProposalState::Rejected, by, reason)
            .await?;
        self.discard_candidates(key_of(work_item), None).await?;
        Ok(v)
    }
}

/// Fingerprint of what a proposal would accept: every proposed version plus proposed edges.
pub fn proposals_fingerprint(p: &Value) -> String {
    let mut parts: Vec<String> = p["proposals"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, x)| format!("{k}:{}", x["version"].as_str().unwrap_or("")))
                .collect()
        })
        .unwrap_or_default();
    parts.sort();
    let edges = kodehub_core::canonical_json(&p["edges"]);
    blake3::hash(format!("{}|{edges}", parts.join(",")).as_bytes()).to_hex()[..16].to_string()
}
