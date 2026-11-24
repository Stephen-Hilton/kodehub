//! Changesets (spec §5.8) — a work item's candidate versions, built, tested and promoted
//! together. Proposed versions are real (immutable, content-addressed) version nodes linked by
//! `proposes`, and each existing node gets a *candidate* `has_version` edge (no `validFrom`), so
//! the stable version is never disturbed. `merge` promotes every candidate in one transaction:
//! all or nothing.
//!
//! State names are the v0.05 ones (D-044): draft/planned ≈ open, approved ≈ submitted,
//! building, reviewing, testing, ready, merged ≈ promoted, rejected.

use std::collections::{BTreeMap, BTreeSet};

use kodehub_core::infer::references;
use kodehub_core::model::{
    now_iso, ChangesetState, CodeEdgeKind, Kind, Language, VersionContent, Visibility,
};
use kodehub_core::{CodeEdge, CodeGraph, CodeSymbol};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::store::{edge_key, key_of, new_key, version_content_from_doc};
use crate::{Result, Store, StoreError};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProposeVersion {
    /// Existing symbol key; omit to propose a new symbol.
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
    /// Component key that should contain a new symbol.
    #[serde(default)]
    pub component: Option<String>,
    /// Class symbol key for a new method.
    #[serde(default)]
    pub member_of: Option<String>,
    /// Requirement keys a (test) symbol verifies.
    #[serde(default)]
    pub verifies: Vec<String>,
    /// Code symbol keys a test exercises (tested_by edges).
    #[serde(default)]
    pub tests_for: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MergeChecks {
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
}

impl MergeChecks {
    pub fn ok(&self) -> bool {
        self.tests_ok
            && self.review_ok
            && self.requirements_covered
            && self.orphans_ok.unwrap_or(true)
    }
}

/// Key a new symbol will get on merge.
pub fn new_symbol_key(language: Language, display_name: &str) -> String {
    match language {
        Language::Rust => display_name.replace("::", "-"),
        Language::Python => format!("py-{}", display_name.replace('.', "-")),
    }
}

const MERGE_WRITE: &[&str] = &[
    "symbols",
    "versions",
    "has_version",
    "calls",
    "depends_on",
    "member_of",
    "tested_by",
    "verifies",
    "contains",
    "satisfies",
    "justified_by",
    "binds_to",
    "changesets",
    "requirements",
];

impl Store {
    pub async fn open_changeset(
        &self,
        title: &str,
        intent: &str,
        requirements: &[String],
        created_by: &str,
    ) -> Result<Value> {
        let key = new_key("cs");
        let now = now_iso();
        let reqs: Vec<String> = requirements.iter().map(|r| key_of(r).to_string()).collect();
        let mut addresses = serde_json::Map::new();
        for r in &reqs {
            let d = self.db.get("requirements", r, None).await?;
            addresses.insert(r.clone(), d["revision"].clone());
        }
        let doc = json!({
            "addresses": addresses,
            "_key": key,
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
        let d = self.db.insert("changesets", &doc, None).await?;
        for r in &reqs {
            self.db
                .insert_ignore(
                    "proposes",
                    &json!({"_key": edge_key("proposes", &format!("changesets/{key}"), &format!("requirements/{r}")),
                            "_from": format!("changesets/{key}"), "_to": format!("requirements/{r}")}),
                    None,
                )
                .await?;
        }
        Ok(d)
    }

    pub async fn get_changeset(&self, key: &str) -> Result<Value> {
        self.db.get("changesets", key_of(key), None).await
    }

    pub async fn list_changesets(&self, open_only: bool) -> Result<Vec<Value>> {
        self.aql(
            "FOR c IN @@changesets FILTER !@open OR c.state NOT IN ['merged', 'rejected']
             SORT c.created_at DESC
             RETURN UNSET(c, 'transcripts')",
            json!({"open": open_only}),
        )
        .await
    }

    pub async fn patch_changeset(&self, key: &str, patch: Value) -> Result<Value> {
        let mut p = patch;
        p["updated_at"] = json!(now_iso());
        self.db.patch("changesets", key_of(key), &p, None).await
    }

    fn state_of(cs: &Value) -> Result<ChangesetState> {
        serde_json::from_value(cs["state"].clone())
            .map_err(|_| StoreError::Invalid(format!("bad changeset state {}", cs["state"])))
    }

    /// Validated state transition with history.
    pub async fn transition(
        &self,
        key: &str,
        to: ChangesetState,
        by: &str,
        note: &str,
    ) -> Result<Value> {
        let cs = self.get_changeset(key).await?;
        let from = Self::state_of(&cs)?;
        if !from.can_transition_to(to) {
            return Err(StoreError::Invalid(format!(
                "changeset {key}: cannot go from {} to {}",
                from.as_str(),
                to.as_str()
            )));
        }
        let mut history = cs["history"].as_array().cloned().unwrap_or_default();
        history.push(json!({"state": to.as_str(), "at": now_iso(), "by": by, "note": note}));
        let mut patch = json!({"state": to.as_str(), "history": history});
        if to == ChangesetState::Building
            && matches!(
                from,
                ChangesetState::Reviewing | ChangesetState::Testing | ChangesetState::Ready
            )
        {
            patch["rounds"] = json!(cs["rounds"].as_i64().unwrap_or(0) + 1);
        }
        self.patch_changeset(key, patch).await
    }

    pub async fn set_plan(&self, key: &str, plan: Value, by: &str) -> Result<Value> {
        // the plan addresses the requirements' revisions as they are now
        let cs = self.get_changeset(key).await?;
        let mut addresses = serde_json::Map::new();
        for r in cs["requirements"].as_array().cloned().unwrap_or_default() {
            if let Some(r) = r.as_str() {
                let d = self.db.get("requirements", r, None).await?;
                addresses.insert(r.to_string(), d["revision"].clone());
            }
        }
        self.patch_changeset(key, json!({"plan": plan, "addresses": addresses}))
            .await?;
        self.transition(key, ChangesetState::Planned, by, "plan attached")
            .await
    }

    /// Append an activity entry (agent log shown in the UI) and optionally a transcript entry.
    pub async fn log_activity(
        &self,
        key: &str,
        entry: Value,
        transcript: Option<Value>,
    ) -> Result<()> {
        let cs = self.get_changeset(key).await?;
        let mut act = cs["activity"].as_array().cloned().unwrap_or_default();
        let mut e = entry;
        e["at"] = json!(now_iso());
        act.push(e);
        let mut patch = json!({"activity": act});
        if let Some(t) = transcript {
            let mut tr = cs["transcripts"].as_array().cloned().unwrap_or_default();
            tr.push(t);
            patch["transcripts"] = json!(tr);
        }
        self.patch_changeset(key, patch).await.map(|_| ())
    }

    fn ensure_editable(cs: &Value) -> Result<()> {
        let st = Self::state_of(cs)?;
        if st.is_terminal() {
            return Err(StoreError::Invalid(format!(
                "changeset is {}; no further proposals",
                st.as_str()
            )));
        }
        Ok(())
    }

    /// Propose a version (of an existing or a new symbol) inside a changeset.
    pub async fn propose_version(&self, key: &str, p: &ProposeVersion) -> Result<Value> {
        let cs = self.get_changeset(key).await?;
        Self::ensure_editable(&cs)?;
        let (pkey, kind, language, display_name, symbol_key, is_new) = match &p.symbol {
            Some(s) => {
                let sym = self.db.get("symbols", key_of(s), None).await?;
                let kind: Kind = serde_json::from_value(sym["kind"].clone()).unwrap();
                let lang: Language = serde_json::from_value(sym["language"].clone()).unwrap();
                (
                    key_of(s).to_string(),
                    kind,
                    lang,
                    sym["display_name"].as_str().unwrap_or_default().to_string(),
                    key_of(s).to_string(),
                    false,
                )
            }
            None => {
                let name = p
                    .display_name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .ok_or_else(|| StoreError::Invalid("new symbols need display_name".into()))?;
                let kind = p
                    .kind
                    .ok_or_else(|| StoreError::Invalid("new symbols need kind".into()))?;
                let lang = p
                    .language
                    .ok_or_else(|| StoreError::Invalid("new symbols need language".into()))?;
                let sk = new_symbol_key(lang, &name);
                // a symbol with that name may already exist → treat as modification
                let existing = self
                    .aql(
                        "FOR s IN @@symbols FILTER s.language == @l AND s.display_name == @n RETURN s._key",
                        json!({"l": lang, "n": name}),
                    )
                    .await?;
                if let Some(k) = existing.first().and_then(Value::as_str) {
                    let doc = self.db.get("symbols", k, None).await?;
                    if doc["kind"] != json!(kind) {
                        return Err(StoreError::Invalid(format!(
                            "`{name}` already exists as a {}; a {} can't replace it",
                            doc["kind"].as_str().unwrap_or("?"),
                            kind.as_str()
                        )));
                    }
                    (k.to_string(), kind, lang, name, k.to_string(), false)
                } else {
                    (sk.clone(), kind, lang, name, sk, true)
                }
            }
        };
        let content = VersionContent {
            kind,
            language,
            signature: p.signature.clone(),
            body: p.body.clone(),
            attributes: p.attributes.clone().unwrap_or_else(|| json!({})),
        };
        let n = content.normalized();
        let hash = n.hash();
        self.db
            .insert_ignore(
                "versions",
                &json!({
                    "_key": hash, "content_hash": hash, "kind": n.kind, "language": n.language,
                    "signature": n.signature, "body": n.body, "attributes": n.attributes,
                    "created_at": now_iso(), "created_by": p.by.clone().unwrap_or_else(|| "agent".into()),
                }),
                None,
            )
            .await?;
        let cs_id = format!("changesets/{}", key_of(key));
        self.db
            .insert_ignore(
                "proposes",
                &json!({"_key": edge_key("proposes", &cs_id, &format!("versions/{hash}")),
                        "_from": cs_id, "_to": format!("versions/{hash}")}),
                None,
            )
            .await?;
        let base_version: Option<String> = if is_new {
            None
        } else {
            self.stable_version_in(&format!("symbols/{symbol_key}"), None)
                .await?
        };
        let candidate_seq = if is_new {
            None
        } else {
            Some(
                self.add_candidate(
                    &format!("symbols/{symbol_key}"),
                    "versions",
                    &hash,
                    key_of(key),
                    p.by.as_deref().unwrap_or("agent"),
                )
                .await?,
            )
        };
        let proposal = json!({
            "base_version": base_version,
            "symbol": symbol_key,
            "new": is_new,
            "display_name": display_name,
            "kind": kind,
            "language": language,
            "visibility": p.visibility.unwrap_or_default(),
            "version": hash,
            "signature": n.signature,
            "body": n.body,
            "attributes": n.attributes,
            "component": p.component,
            "member_of": p.member_of,
            "verifies": p.verifies.iter().map(|r| key_of(r).to_string()).collect::<Vec<_>>(),
            "tests_for": p.tests_for.iter().map(|r| key_of(r).to_string()).collect::<Vec<_>>(),
            "description": p.description,
            "by": p.by.clone().unwrap_or_else(|| "agent".into()),
            "round": cs["rounds"],
            "candidate_seq": candidate_seq,
            "at": now_iso(),
        });
        let mut proposals = cs["proposals"].as_object().cloned().unwrap_or_default();
        proposals.insert(pkey.clone(), proposal.clone());
        self.patch_changeset(key, json!({"proposals": proposals}))
            .await?;
        Ok(proposal)
    }

    /// Withdraw a proposal.
    pub async fn withdraw_proposal(&self, key: &str, symbol: &str) -> Result<()> {
        let cs = self.get_changeset(key).await?;
        Self::ensure_editable(&cs)?;
        let mut proposals = cs["proposals"].as_object().cloned().unwrap_or_default();
        proposals.remove(symbol);
        self.discard_candidates(key_of(key), Some(&format!("symbols/{}", key_of(symbol))))
            .await?;
        self.patch_changeset(key, json!({"proposals": proposals}))
            .await
            .map(|_| ())
    }

    pub async fn propose_edge(
        &self,
        key: &str,
        collection: &str,
        from: &str,
        to: &str,
        data: Value,
    ) -> Result<Value> {
        let cs = self.get_changeset(key).await?;
        Self::ensure_editable(&cs)?;
        crate::store::check_edge_collection(collection)?;
        let mut edges = cs["edges"].as_array().cloned().unwrap_or_default();
        let e = json!({"collection": collection, "from": from, "to": to, "data": data});
        if !edges.contains(&e) {
            edges.push(e.clone());
        }
        self.patch_changeset(key, json!({"edges": edges})).await?;
        Ok(e)
    }

    /// Store a review round. Findings get ids and `status: open`.
    pub async fn submit_review(
        &self,
        key: &str,
        findings: &[Value],
        summary: &str,
        by: &str,
        source: &str,
    ) -> Result<Value> {
        let cs = self.get_changeset(key).await?;
        Self::ensure_editable(&cs)?;
        let mut reviews = cs["reviews"].as_array().cloned().unwrap_or_default();
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
        // reviewer or a human in the UI can produce a review that satisfies invariant 6.
        let review = json!({"round": round, "findings": fs, "summary": summary, "by": by, "at": now_iso(),
                            "source": source, "fingerprint": proposals_fingerprint(&cs)});
        reviews.push(review.clone());
        self.patch_changeset(key, json!({"reviews": reviews}))
            .await?;
        Ok(review)
    }

    /// Update the status/resolution of findings in the latest review.
    pub async fn resolve_findings(
        &self,
        key: &str,
        updates: &BTreeMap<String, (String, String)>,
    ) -> Result<()> {
        let cs = self.get_changeset(key).await?;
        let mut reviews = cs["reviews"].as_array().cloned().unwrap_or_default();
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
        self.patch_changeset(key, json!({"reviews": reviews}))
            .await
            .map(|_| ())
    }

    /// The current graph with a changeset's proposals and edges applied.
    pub async fn overlay_graph(&self, key: &str) -> Result<(CodeGraph, Vec<String>)> {
        let cs = self.get_changeset(key).await?;
        let mut g = self.load_code_graph().await?;
        let proposals = cs["proposals"].as_object().cloned().unwrap_or_default();
        let hashes: Vec<String> = proposals
            .values()
            .filter_map(|p| p["version"].as_str().map(String::from))
            .collect();
        let versions = self.load_versions(&hashes).await?;
        let mut touched = Vec::new();
        for (pkey, p) in &proposals {
            let hash = p["version"].as_str().unwrap_or_default().to_string();
            let content = versions
                .get(&hash)
                .cloned()
                .ok_or_else(|| StoreError::NotFound(format!("proposed version {hash}")))?;
            match g.symbols.get_mut(pkey) {
                Some(s) => s.version = hash.clone(),
                None => {
                    g.symbols.insert(
                        pkey.clone(),
                        CodeSymbol {
                            id: pkey.clone(),
                            display_name: p["display_name"].as_str().unwrap_or_default().into(),
                            kind: content.kind,
                            language: content.language,
                            visibility: serde_json::from_value(p["visibility"].clone())
                                .unwrap_or_default(),
                            tags: vec![],
                            version: hash.clone(),
                        },
                    );
                }
            }
            g.versions.insert(hash, content);
            touched.push(pkey.clone());
        }
        // edges: member_of first (affects method name resolution), then inferred references
        for (pkey, p) in &proposals {
            if let Some(class) = p["member_of"].as_str() {
                g.edges.push(CodeEdge {
                    from: pkey.clone(),
                    to: key_of(class).into(),
                    kind: CodeEdgeKind::MemberOf,
                    pinned_version: None,
                });
            }
        }
        for (pkey, p) in &proposals {
            let lang: Language =
                serde_json::from_value(p["language"].clone()).unwrap_or(Language::Rust);
            let text = format!(
                "{}\n{}",
                p["signature"].as_str().unwrap_or(""),
                p["body"].as_str().unwrap_or("")
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
            if p["kind"] == json!("test") {
                let mut targets: Vec<String> = p["tests_for"]
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
                            kind: CodeEdgeKind::TestedBy,
                            pinned_version: None,
                        });
                    }
                }
            }
        }
        for e in cs["edges"].as_array().cloned().unwrap_or_default() {
            if let Some(kind) =
                CodeEdgeKind::from_collection(e["collection"].as_str().unwrap_or(""))
            {
                let (f, t) = (
                    e["from"].as_str().unwrap_or(""),
                    e["to"].as_str().unwrap_or(""),
                );
                if f.starts_with("symbols/") && t.starts_with("symbols/") {
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

    /// Tests that must pass for this changeset: tests reaching any touched symbol, tests that
    /// verify its requirements, and new tests.
    pub async fn affected_tests(
        &self,
        key: &str,
        g: &CodeGraph,
        touched: &[String],
    ) -> Result<Vec<String>> {
        let cs = self.get_changeset(key).await?;
        let mut out: BTreeSet<String> = BTreeSet::new();
        for t in touched {
            out.extend(g.tests_reaching(t));
        }
        let reqs: Vec<String> = cs["requirements"]
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
                "FOR e IN @@verifies FILTER e._to IN @r RETURN PARSE_IDENTIFIER(e._from).key",
                json!({"r": reqs}),
            )
            .await?;
        out.extend(verifying.iter().filter_map(Value::as_str).map(String::from));
        for (pk, p) in cs["proposals"].as_object().cloned().unwrap_or_default() {
            if p["kind"] == json!("test") {
                out.insert(pk);
            }
        }
        Ok(out
            .into_iter()
            .filter(|t| g.symbols.contains_key(t))
            .collect())
    }

    /// Compute merge checks (tests + review); orphans are checked inside the merge transaction.
    pub async fn merge_checks(&self, key: &str) -> Result<(MergeChecks, CodeGraph, Vec<String>)> {
        let cs = self.get_changeset(key).await?;
        let (g, touched) = self.overlay_graph(key).await?;
        let affected = self.affected_tests(key, &g, &touched).await?;
        let mut failing = Vec::new();
        let mut missing = Vec::new();
        for t in &affected {
            let s = g.symbol(t)?;
            let closure = g.closure_hash(t)?;
            let rows = self
                .aql(
                    "FOR r IN @@test_runs FILTER r.test_version == @v AND r.closure_hash == @c SORT r.ran_at DESC LIMIT 1 RETURN r.status",
                    json!({"v": s.version, "c": closure}),
                )
                .await?;
            match rows.first().and_then(Value::as_str) {
                Some("pass") => {}
                Some(_) => failing.push(t.clone()),
                None => missing.push(t.clone()),
            }
        }
        let reviews = cs["reviews"].as_array().cloned().unwrap_or_default();
        // Invariant 6: the passing review must be of exactly what would be merged.
        let review_current = reviews
            .last()
            .map(|r| r["fingerprint"] == json!(proposals_fingerprint(&cs)))
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
        // only an attested review counts (engine reviewer or a human in the UI)
        let attested = reviews
            .last()
            .map(|r| {
                matches!(
                    r["source"].as_str(),
                    Some("engine_reviewer") | Some("human")
                )
            })
            .unwrap_or(false);
        // requirement edits need at least one proposed test verifying each requirement
        let mut uncovered_reqs = Vec::new();
        // requirement-centric (final review #7): any changeset listing a requirement whose
        // revision is newer than its covered revision must bring a test that verifies it
        let mut needs_test: Vec<Value> = Vec::new();
        for r in cs["requirements"].as_array().cloned().unwrap_or_default() {
            if let Some(rk) = r.as_str() {
                let d = self.db.get("requirements", rk, None).await?;
                let rev = d["revision"].as_i64().unwrap_or(1);
                let covered = d["covered_revision"].as_i64().unwrap_or(rev);
                if rev > covered {
                    needs_test.push(r.clone());
                }
            }
        }
        {
            for r in needs_test.clone() {
                let covered = cs["proposals"]
                    .as_object()
                    .map(|o| {
                        o.values().any(|p| {
                            p["kind"] == "test"
                                && p["verifies"]
                                    .as_array()
                                    .map(|v| v.contains(&r))
                                    .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false);
                if !covered {
                    uncovered_reqs.push(r.as_str().unwrap_or("").to_string());
                }
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
        let checks = MergeChecks {
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
        };
        Ok((checks, g, touched))
    }

    /// Merge. With `dry_run` the transaction is aborted after the orphan check, so callers can
    /// see whether a merge *would* succeed.
    pub async fn merge_changeset(&self, key: &str, by: &str, dry_run: bool) -> Result<MergeChecks> {
        // one merge/deploy at a time: checks and writes see a stable graph (review M6 #3)
        let _write = self.write_lock.lock().await;
        let key = key_of(key).to_string();
        let cs = self.get_changeset(&key).await?;
        let state = Self::state_of(&cs)?;
        if !dry_run && state != ChangesetState::Ready {
            return Err(StoreError::Invalid(format!(
                "changeset is {}, not ready",
                state.as_str()
            )));
        }
        let (mut checks, g, touched) = self.merge_checks(&key).await?;
        if !dry_run && !(checks.tests_ok && checks.review_ok && checks.requirements_covered) {
            return Err(StoreError::Conflict(format!(
                "merge blocked: tests_ok={} (failing {:?}, not run {:?}), review_ok={} (attested {}, open blocking {}), requirements_covered={} {:?}",
                checks.tests_ok, checks.failing_tests, checks.missing_tests, checks.review_ok, checks.review_attested,
                checks.open_blocking.len(), checks.requirements_covered, checks.uncovered_requirements
            )));
        }
        let orphans_before: BTreeSet<String> = self
            .orphans()
            .await?
            .iter()
            .filter_map(|o| o["key"].as_str().map(String::from))
            .collect();
        let proposals = cs["proposals"].as_object().cloned().unwrap_or_default();
        let trx = self.db.begin(MERGE_WRITE, &[]).await?;
        // existing pins on touched symbols' edges survive the merge (review M6 #10)
        let current_pins: BTreeMap<(String, String, CodeEdgeKind), String> = self
            .load_code_graph()
            .await?
            .edges
            .into_iter()
            .filter_map(|e| {
                e.pinned_version
                    .clone()
                    .map(|p| ((e.from, e.to, e.kind), p))
            })
            .collect();
        let res: Result<BTreeMap<String, String>> = async {
            // re-read inside the transaction: a concurrent reject must win (review M6 #12)
            let in_trx = self.db.get("changesets", &key, Some(&trx)).await?;
            if !dry_run && in_trx["state"] != json!("ready") {
                return Err(StoreError::Conflict(format!("changeset is {} — merge aborted", in_trx["state"])));
            }
            let mut merged = BTreeMap::new();
            // 1. symbols + versions (each proposal must still be based on the current version)
            for (pkey, p) in &proposals {
                if p["new"] != json!(true) {
                    let cur = self
                        .stable_version_in(&format!("symbols/{pkey}"), Some(&trx))
                        .await?;
                    let base = p["base_version"].as_str();
                    if base.is_some() && cur.as_deref() != base {
                        return Err(StoreError::Conflict(format!(
                            "`{}` changed since it was proposed; re-propose against the current version",
                            p["display_name"].as_str().unwrap_or(pkey)
                        )));
                    }
                }
                let hash = p["version"].as_str().unwrap_or_default().to_string();
                let vdoc = self.db.get("versions", &hash, Some(&trx)).await?;
                let content = version_content_from_doc(&vdoc)?;
                let exists = self.db.get("symbols", pkey, Some(&trx)).await.is_ok();
                if exists && p["new"] == json!(true) {
                    return Err(StoreError::Conflict(format!(
                        "`{}` was created by another change since this was proposed; re-propose against it",
                        p["display_name"].as_str().unwrap_or(pkey)
                    )));
                }
                if exists {
                    let sym = self.db.get("symbols", pkey, Some(&trx)).await?;
                    if sym["kind"] != json!(content.kind) || sym["language"] != json!(content.language) {
                        return Err(StoreError::Invalid(format!("version kind/language must match symbol `{pkey}`")));
                    }
                    self.promote_in(&format!("symbols/{pkey}"), "versions", &hash, Some(&key), by, &trx).await?;
                } else {
                    self.db.insert("symbols", &json!({
                        "_key": pkey, "display_name": p["display_name"], "kind": p["kind"], "language": p["language"],
                        "visibility": p["visibility"], "tags": [], "description": p["description"],
                        "created_at": now_iso(), "created_by": by, "changeset": key,
                    }), Some(&trx)).await.map_err(|e| if e.is_unique_violation() {
                        StoreError::Conflict(format!("display name `{}` already taken", p["display_name"]))
                    } else { e })?;
                    self.promote_in(&format!("symbols/{pkey}"), "versions", &hash, Some(&key), by, &trx).await?;
                }
                merged.insert(pkey.clone(), hash);
            }
            // 2. code edges of touched symbols, from the overlay graph
            for pkey in &touched {
                let sid = format!("symbols/{pkey}");
                for coll in ["calls", "depends_on"] {
                    self.db.aql("FOR e IN @@c FILTER e._from == @s REMOVE e IN @@c", json!({"@c": coll, "s": sid}), Some(&trx)).await?;
                }
            }
            let touched_set: BTreeSet<&String> = touched.iter().collect();
            for e in &g.edges {
                if touched_set.contains(&e.from) || touched_set.contains(&e.to) {
                    let coll = e.kind.collection();
                    let (f, t) = (format!("symbols/{}", e.from), format!("symbols/{}", e.to));
                    let pin = e.pinned_version.clone().or_else(|| current_pins.get(&(e.from.clone(), e.to.clone(), e.kind)).cloned());
                    let mut doc = json!({"_key": edge_key(coll, &f, &t), "_from": f, "_to": t});
                    if let Some(p) = pin {
                        doc["pinned_version"] = json!(p);
                    }
                    self.db.insert_ignore(coll, &doc, Some(&trx)).await?;
                }
            }
            // explicit code edges proposed between any symbols
            for e in cs["edges"].as_array().cloned().unwrap_or_default() {
                let coll = e["collection"].as_str().unwrap_or_default();
                if CodeEdgeKind::from_collection(coll).is_some() {
                    let (f, t) = (e["from"].as_str().unwrap_or_default(), e["to"].as_str().unwrap_or_default());
                    self.db.insert_ignore(coll, &json!({"_key": edge_key(coll, f, t), "_from": f, "_to": t}), Some(&trx)).await?;
                }
            }
            // 3. verifies + contains for proposals
            for (pkey, p) in &proposals {
                let sid = format!("symbols/{pkey}");
                for r in p["verifies"].as_array().cloned().unwrap_or_default() {
                    let rid = format!("requirements/{}", r.as_str().unwrap_or_default());
                    self.db.insert_ignore("verifies", &json!({"_key": edge_key("verifies", &sid, &rid), "_from": sid, "_to": rid}), Some(&trx)).await?;
                }
                if p["new"] == json!(true) {
                    let comp = match p["component"].as_str() {
                        Some(c) if !c.is_empty() => Some(format!("components/{}", key_of(c))),
                        _ => {
                            // inherit the component of the first related symbol
                            let related: Vec<String> = g.edges.iter()
                                .filter(|e| e.from == *pkey || e.to == *pkey)
                                .map(|e| format!("symbols/{}", if e.from == *pkey { &e.to } else { &e.from }))
                                .collect();
                            self.db.aql("FOR e IN @@contains FILTER e._to IN @r AND STARTS_WITH(e._from, CONCAT(@pfx, 'components/')) LIMIT 1 RETURN e._from",
                                json!({"r": related}), Some(&trx)).await?.first().and_then(Value::as_str).map(String::from)
                        }
                    };
                    if let Some(c) = comp {
                        self.db.insert_ignore("contains", &json!({"_key": edge_key("contains", &c, &sid), "_from": c, "_to": sid}), Some(&trx)).await?;
                    }
                }
            }
            // 4. explicit non-code edges
            for e in cs["edges"].as_array().cloned().unwrap_or_default() {
                let coll = e["collection"].as_str().unwrap_or_default();
                if CodeEdgeKind::from_collection(coll).is_some() {
                    continue; // already applied from the overlay graph
                }
                let (f, t) = (e["from"].as_str().unwrap_or_default(), e["to"].as_str().unwrap_or_default());
                let mut d = e["data"].clone();
                if !d.is_object() { d = json!({}); }
                d["_key"] = json!(edge_key(coll, f, t));
                d["_from"] = json!(f);
                d["_to"] = json!(t);
                self.db.insert_ignore(coll, &d, Some(&trx)).await?;
            }
            self.refresh_closure_hashes(Some(&trx)).await?;
            Ok(merged)
        }
        .await;
        let merged = match res {
            Ok(m) => m,
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                return Err(e);
            }
        };
        let after: BTreeSet<String> = self
            .orphans_in(Some(&trx))
            .await?
            .iter()
            .filter_map(|o| o["key"].as_str().map(String::from))
            .collect();
        checks.new_orphans = after.difference(&orphans_before).cloned().collect();
        checks.orphans_ok = Some(checks.new_orphans.is_empty());
        if dry_run || !checks.new_orphans.is_empty() {
            self.db.abort(&trx).await?;
            if !dry_run {
                return Err(StoreError::Conflict(format!(
                    "merge would introduce orphans: {:?}",
                    checks.new_orphans
                )));
            }
            return Ok(checks);
        }
        let finish: Result<()> = async {
            // a merge covers each listed requirement up to the revision it addressed; at_risk is
            // derived from (revision > covered_revision), never cleared blindly (final review #7)
            for r in cs["requirements"].as_array().cloned().unwrap_or_default() {
                if let Some(r) = r.as_str() {
                    let doc = self.db.get("requirements", r, Some(&trx)).await?;
                    let rev = doc["revision"].as_i64().unwrap_or(1);
                    let prev = doc["covered_revision"].as_i64().unwrap_or(rev);
                    let addressed = cs["addresses"][r].as_i64().unwrap_or(rev);
                    let covered = prev.max(addressed.min(rev));
                    self.db.patch("requirements", r, &json!({"covered_revision": covered, "at_risk": rev > covered}), Some(&trx)).await?;
                }
            }
            let in_trx = self.db.get("changesets", &key, Some(&trx)).await?;
            let mut history = in_trx["history"].as_array().cloned().unwrap_or_default();
            history.push(json!({"state": "merged", "at": now_iso(), "by": by, "note": "every candidate promoted in one transaction"}));
            self.db.patch("changesets", &key, &json!({
                "state": "merged", "merged_at": now_iso(), "merged_by": by, "merged_versions": merged,
                "history": history, "checks": checks, "updated_at": now_iso(),
            }), Some(&trx)).await?;
            Ok(())
        }
        .await;
        match finish {
            Ok(()) => {
                self.db.commit(&trx).await?;
                // named open releases move to the promoted versions (spec §5.8)
                let swaps: Vec<crate::releases::Selected> = merged
                    .iter()
                    .map(|(k, v)| crate::releases::code_swap(k, v))
                    .collect();
                for r in cs["releases"].as_array().cloned().unwrap_or_default() {
                    if let Some(r) = r.as_str() {
                        if let Err(e) = self.swap_release_versions(r, &swaps, by).await {
                            tracing::warn!("changeset {key}: release {r} not updated: {e}");
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

    pub async fn reject_changeset(&self, key: &str, by: &str, reason: &str) -> Result<Value> {
        let v = self
            .transition(key, ChangesetState::Rejected, by, reason)
            .await?;
        self.discard_candidates(key_of(key), None).await?;
        let cs = self.get_changeset(key).await?;
        for r in cs["requirements"].as_array().cloned().unwrap_or_default() {
            if let Some(r) = r.as_str() {
                // still at risk unless another open changeset addresses it — leave the flag
                let _ = r;
            }
        }
        Ok(v)
    }
}

/// Fingerprint of what a changeset would merge: every proposal's version plus proposed edges.
pub fn proposals_fingerprint(cs: &Value) -> String {
    let mut parts: Vec<String> = cs["proposals"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, p)| format!("{k}:{}", p["version"].as_str().unwrap_or("")))
                .collect()
        })
        .unwrap_or_default();
    parts.sort();
    let edges = kodehub_core::canonical_json(&cs["edges"]);
    blake3::hash(format!("{}|{edges}", parts.join(",")).as_bytes()).to_hex()[..16].to_string()
}
