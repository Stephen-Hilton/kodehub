//! The changeset workflow (doc 04), driven by changeset state so it can resume after a crash or
//! an error: draft → planned → approved → building → reviewing → testing → ready → merged, with
//! rework loops (max 3) and escalation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kodehub_core::config::Config;
use kodehub_core::model::{now_iso, ChangesetState, Kind, Language};
use kodehub_runner::{BuildTarget, Runner};
use kodehub_store::changesets::ProposeVersion;
use kodehub_store::{key_of, Store};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::backend::claude_code::ClaudeCodeBackend;
use crate::context::*;
use crate::{
    prompts, schemas, AgentBackend, AgentError, AgentOutput, AgentTask, ApiBackend, Emitter,
    RecordedBackend, Result, Role,
};

#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Skip human approval of plans.
    pub auto_approve: bool,
    /// Merge as soon as a changeset is ready.
    pub auto_merge: bool,
    /// Maximum rework rounds before escalation.
    pub max_rounds: i64,
    /// Pre-build run-now harnesses after merge so "run now" is instant.
    pub warm_cache: bool,
}

impl Default for EngineOptions {
    fn default() -> Self {
        EngineOptions {
            auto_approve: false,
            auto_merge: true,
            max_rounds: 3,
            warm_cache: true,
        }
    }
}

pub struct Engine {
    pub store: Store,
    pub runner: Runner,
    pub config: Arc<RwLock<Config>>,
    pub server_url: String,
    pub kodehub_bin: PathBuf,
    pub wrapper: PathBuf,
    pub emit: Emitter,
    pub opts: EngineOptions,
    backend_override: Option<Arc<dyn AgentBackend>>,
    recorded: Mutex<Option<(String, Arc<RecordedBackend>)>>,
    status: Mutex<BTreeMap<Role, Value>>,
    running: Mutex<BTreeSet<String>>,
}

/// Default recording shipped with the repo (the doc 08 demo).
pub fn default_recording() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("recordings/demo-locked-account.json")
}

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        runner: Runner,
        config: Arc<RwLock<Config>>,
        server_url: String,
        kodehub_bin: PathBuf,
        wrapper: PathBuf,
        emit: Emitter,
        opts: EngineOptions,
    ) -> Self {
        Engine {
            store,
            runner,
            config,
            server_url,
            kodehub_bin,
            wrapper,
            emit,
            opts,
            backend_override: None,
            recorded: Mutex::new(None),
            status: Mutex::new(BTreeMap::new()),
            running: Mutex::new(BTreeSet::new()),
        }
    }

    /// Force a backend (tests).
    pub fn with_backend(mut self, b: Arc<dyn AgentBackend>) -> Self {
        self.backend_override = Some(b);
        self
    }

    pub async fn backend(&self) -> Result<Arc<dyn AgentBackend>> {
        if let Some(b) = &self.backend_override {
            return Ok(b.clone());
        }
        let cfg = self.config.read().await.clone();
        match cfg.agents.backend.as_str() {
            "api" => Ok(Arc::new(ApiBackend::from_env(&self.server_url)?)),
            "recorded" => {
                let path = cfg
                    .agents
                    .recording
                    .clone()
                    .map(PathBuf::from)
                    .unwrap_or_else(default_recording);
                let key = path.to_string_lossy().to_string();
                let mut g = self.recorded.lock().unwrap();
                // an exhausted recording fails loudly instead of silently replaying (review M6 #15);
                // re-selecting the recorded backend in settings resets it
                if let Some((k, b)) = g.as_ref() {
                    if *k == key {
                        return Ok(b.clone());
                    }
                }
                let b = Arc::new(RecordedBackend::from_file(&path)?);
                *g = Some((key, b.clone()));
                Ok(b)
            }
            _ => Ok(Arc::new(ClaudeCodeBackend {
                wrapper: self.wrapper.clone(),
                token_env: cfg.agents.token_env.clone(),
                kodehub_bin: self.kodehub_bin.clone(),
                server_url: self.server_url.clone(),
                workdir: self.runner.home.join("agents"),
                timeout: Duration::from_secs(cfg.agents.timeout_secs),
            })),
        }
    }

    /// Forget the loaded recording (next task reloads it from the start).
    pub fn reset_recording(&self) {
        *self.recorded.lock().unwrap() = None;
    }

    pub fn agent_status(&self) -> Value {
        let st = self.status.lock().unwrap();
        let agents: Vec<Value> = Role::ALL
            .iter()
            .map(|r| {
                st.get(r)
                    .cloned()
                    .unwrap_or_else(|| json!({"role": r.as_str(), "state": "idle"}))
            })
            .collect();
        json!({"agents": agents})
    }

    fn set_status(&self, role: Role, state: &str, label: &str, cs: Option<&str>) {
        let v = json!({"role": role.as_str(), "state": state, "task": label, "changeset": cs, "since": now_iso()});
        self.status.lock().unwrap().insert(role, v.clone());
        (self.emit)("agent.activity", v);
    }

    async fn activity(&self, cs: &str, role: &str, message: &str) {
        let _ = self
            .store
            .log_activity(cs, json!({"role": role, "message": message}), None)
            .await;
        (self.emit)(
            "agent.activity",
            json!({"role": role, "changeset": cs, "message": message}),
        );
        (self.emit)("changeset.updated", json!({"key": cs}));
    }

    /// Run one agent task and keep its transcript on the changeset.
    async fn call(
        &self,
        role: Role,
        label: &str,
        input: String,
        schema: Value,
        cs: Option<&str>,
    ) -> Result<AgentOutput> {
        let model = self.config.read().await.model_for(role.as_str());
        let task = AgentTask {
            role,
            system: prompts::system_prompt(role),
            input,
            schema,
            model,
            changeset: cs.map(String::from),
            label: label.into(),
        };
        self.set_status(role, "working", label, cs);
        let backend = self.backend().await;
        let res = match backend {
            Ok(b) => b.run(&task).await,
            Err(e) => Err(e),
        };
        self.set_status(role, "idle", label, cs);
        if let Some(cs) = cs {
            let entry = match &res {
                Ok(o) => {
                    json!({"role": role.as_str(), "label": label, "input": task.input, "output": o.json,
                                "transcript": o.transcript, "backend": o.backend, "model": o.model, "duration_ms": o.duration_ms, "at": now_iso()})
                }
                Err(e) => {
                    json!({"role": role.as_str(), "label": label, "input": task.input, "error": e.to_string(), "at": now_iso()})
                }
            };
            let _ = self.store.log_activity(cs, json!({"role": role.as_str(), "message": format!("{label}: {}", if res.is_ok() { "done" } else { "failed" })}), Some(entry)).await;
            (self.emit)("changeset.updated", json!({"key": cs}));
        }
        res
    }

    // ------------------------------------------------------------------ planning

    /// A human edited a requirement: mark at risk (already done by the store), open (or reuse)
    /// a changeset and plan it.
    pub async fn on_requirement_edited(&self, req: &str, by: &str) -> Result<String> {
        let r = self.store.db.get("requirements", req, None).await?;
        let open = self.store.list_changesets(true).await?;
        let existing = open.iter().find(|c| {
            matches!(c["state"].as_str(), Some("draft") | Some("planned"))
                && c["requirements"]
                    .as_array()
                    .map(|a| a.contains(&json!(req)))
                    .unwrap_or(false)
        });
        let key = match existing {
            Some(c) => c["_key"].as_str().unwrap_or_default().to_string(),
            None => {
                let cs = self
                    .store
                    .open_changeset(
                        &format!("Address {} (rev {})", r["title"].as_str().unwrap_or(req), r["revision"]),
                        &format!("Requirement `{req}` was edited by {by}; bring code and tests in line with revision {}.", r["revision"]),
                        &[req.to_string()],
                        "orchestrator",
                    )
                    .await?;
                cs["_key"].as_str().unwrap_or_default().to_string()
            }
        };
        self.store
            .patch_changeset(&key, json!({"origin": "requirement_edit"}))
            .await?;
        (self.emit)("changeset.updated", json!({"key": key, "state": "draft"}));
        self.activity(
            &key,
            "orchestrator",
            &format!("Opened for requirement edit ({req})"),
        )
        .await;
        self.plan(&key).await?;
        Ok(key)
    }

    pub async fn plan(&self, cs_key: &str) -> Result<Value> {
        let cs = self.store.get_changeset(cs_key).await?;
        let reqs: Vec<String> = str_list(&cs["requirements"]);
        let g = self.store.load_code_graph().await?;
        let relevant = relevant_symbols(&self.store, &g, &reqs).await?;
        let components = self
            .store
            .aql(
                "FOR c IN @@components RETURN CONCAT('`', c._key, '` — ', c.description)",
                json!({}),
            )
            .await?;
        let decisions = self.decisions_md(&relevant).await?;
        let input = format!(
            "## Mission\n{}\n\n## Changeset\n**{}** — {}\n\n## Requirements\n{}\n## Decisions\n{}\n## Components\n{}\n\n## Relevant code (current versions)\n{}\n## Task\nProduce the plan for this changeset as JSON: `summary`, `changes` (each with `symbol` = existing key or \"\" for new, `display_name`, `kind`, `language`, `action` add|modify, `component` key for new symbols or \"\", `description`), `tests` (test ideas derived from the requirement), `risks`.",
            mission(&self.store).await?,
            cs["title"].as_str().unwrap_or(""),
            cs["intent"].as_str().unwrap_or(""),
            requirements_md(&self.store, &reqs, true).await?,
            decisions,
            components.iter().filter_map(Value::as_str).map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n"),
            code_md(&g, &relevant, true, true),
        );
        let out = self
            .call(
                Role::Orchestrator,
                "plan",
                input,
                schemas::plan(),
                Some(cs_key),
            )
            .await?;
        let mut plan = out.json.clone();
        // normalise: empty symbol → null, unknown symbol keys → add
        if let Some(changes) = plan["changes"].as_array_mut() {
            for c in changes.iter_mut() {
                let sym = c["symbol"].as_str().unwrap_or("").to_string();
                if sym.is_empty() || !g.symbols.contains_key(key_of(&sym)) {
                    c["symbol"] = Value::Null;
                    if c["action"] == "modify" && !sym.is_empty() {
                        c["action"] = json!("add");
                    }
                } else {
                    c["symbol"] = json!(key_of(&sym));
                }
            }
        }
        let v = self
            .store
            .set_plan(cs_key, plan.clone(), "orchestrator")
            .await?;
        self.activity(
            cs_key,
            "orchestrator",
            &format!("Plan ready: {}", plan["summary"].as_str().unwrap_or("")),
        )
        .await;
        if self.opts.auto_approve {
            self.store
                .transition(
                    cs_key,
                    ChangesetState::Approved,
                    "orchestrator",
                    "auto-approved",
                )
                .await?;
            Box::pin(self.execute(cs_key)).await?;
        }
        Ok(v)
    }

    async fn decisions_md(&self, symbols: &BTreeSet<String>) -> Result<String> {
        let ids: Vec<String> = symbols.iter().map(|s| format!("symbols/{s}")).collect();
        let rows = self
            .store
            .aql(
                "FOR e IN @@justified_by FILTER e._from IN @ids LET d = DOCUMENT(e._to)
                 COLLECT k = d._key, t = d.title, c = d.choice, r = d.rationale
                 RETURN CONCAT('- **', t, '** — ', c, ' (', r, ')')",
                json!({"ids": ids}),
            )
            .await?;
        Ok(if rows.is_empty() {
            "(none)".into()
        } else {
            rows.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    // ------------------------------------------------------------------ execution

    /// Drive an approved changeset to merged / escalated. Safe to call again to resume.
    pub async fn execute(&self, cs_key: &str) -> Result<String> {
        {
            let mut r = self.running.lock().unwrap();
            if !r.insert(cs_key.to_string()) {
                return Ok("already running".into());
            }
        }
        let res = self.drive(cs_key).await;
        self.running.lock().unwrap().remove(cs_key);
        match res {
            Ok(s) => {
                let _ = self
                    .store
                    .patch_changeset(cs_key, json!({"error": null}))
                    .await;
                Ok(s)
            }
            Err(e) => {
                let _ = self
                    .store
                    .patch_changeset(cs_key, json!({"error": e.to_string()}))
                    .await;
                self.activity(cs_key, "orchestrator", &format!("Stopped: {e}"))
                    .await;
                Err(e)
            }
        }
    }

    pub fn is_running(&self, cs_key: &str) -> bool {
        self.running.lock().unwrap().contains(cs_key)
    }

    async fn state(&self, cs_key: &str) -> Result<(Value, ChangesetState)> {
        let cs = self.store.get_changeset(cs_key).await?;
        let st: ChangesetState = serde_json::from_value(cs["state"].clone())
            .map_err(|e| AgentError::Workflow(e.to_string()))?;
        Ok((cs, st))
    }

    async fn to(&self, cs: &str, s: ChangesetState, by: &str, note: &str) -> Result<()> {
        self.store.transition(cs, s, by, note).await?;
        (self.emit)("changeset.updated", json!({"key": cs, "state": s.as_str()}));
        Ok(())
    }

    async fn drive(&self, cs_key: &str) -> Result<String> {
        for _ in 0..40 {
            let (cs, st) = self.state(cs_key).await?;
            let rounds = cs["rounds"].as_i64().unwrap_or(0);
            match st {
                ChangesetState::Draft => {
                    self.plan(cs_key).await?;
                    return Ok("planned".into());
                }
                ChangesetState::Planned => return Ok("awaiting approval".into()),
                ChangesetState::Approved => {
                    self.to(
                        cs_key,
                        ChangesetState::Building,
                        "orchestrator",
                        "build started",
                    )
                    .await?
                }
                ChangesetState::Building => {
                    let has_code = cs["proposals"]
                        .as_object()
                        .map(|o| o.values().any(|p| p["kind"] != "test"))
                        .unwrap_or(false);
                    if cs["skip_to_review"] == json!(true) {
                        // re-approved after out-of-plan changes: proposals are unchanged
                        self.store
                            .patch_changeset(cs_key, json!({"skip_to_review": null}))
                            .await?;
                    } else if !has_code {
                        self.tester(cs_key).await?;
                        let out_of_plan = self.coder(cs_key, &[], "").await?;
                        if !out_of_plan.is_empty() {
                            return self.request_reapproval(cs_key, &out_of_plan).await;
                        }
                    } else {
                        let rework = cs["rework"].clone();
                        let findings = rework["findings"].as_array().cloned().unwrap_or_default();
                        let failures = rework["failures"].as_str().unwrap_or("").to_string();
                        let out_of_plan = self.coder(cs_key, &findings, &failures).await?;
                        if !out_of_plan.is_empty() {
                            self.store
                                .patch_changeset(cs_key, json!({"rework": null}))
                                .await?;
                            return self.request_reapproval(cs_key, &out_of_plan).await;
                        }
                    }
                    self.store
                        .patch_changeset(cs_key, json!({"rework": null}))
                        .await?;
                    self.to(
                        cs_key,
                        ChangesetState::Reviewing,
                        "coder",
                        "proposals ready for review",
                    )
                    .await?;
                }
                ChangesetState::Reviewing => {
                    let open = self.reviewer(cs_key, rounds).await?;
                    if open.is_empty() {
                        self.to(cs_key, ChangesetState::Testing, "reviewer", "review passed")
                            .await?;
                    } else if rounds >= self.opts.max_rounds {
                        return self
                            .escalate(
                                cs_key,
                                &format!(
                                    "{} review finding(s) still open after {} rounds",
                                    open.len(),
                                    rounds
                                ),
                            )
                            .await;
                    } else {
                        self.store
                            .patch_changeset(cs_key, json!({"rework": {"findings": open}}))
                            .await?;
                        self.to(
                            cs_key,
                            ChangesetState::Building,
                            "reviewer",
                            "changes requested",
                        )
                        .await?;
                    }
                }
                ChangesetState::Testing => {
                    let (passed, failures) = self.run_tests(cs_key).await?;
                    if passed {
                        self.record_decisions(cs_key).await?;
                        self.to(
                            cs_key,
                            ChangesetState::Ready,
                            "tester",
                            "all affected tests pass",
                        )
                        .await?;
                    } else if rounds >= self.opts.max_rounds {
                        return self
                            .escalate(
                                cs_key,
                                &format!("tests still failing after {rounds} rounds"),
                            )
                            .await;
                    } else {
                        self.store
                            .patch_changeset(cs_key, json!({"rework": {"failures": failures}}))
                            .await?;
                        self.to(cs_key, ChangesetState::Building, "tester", "tests failing")
                            .await?;
                    }
                }
                ChangesetState::Ready => {
                    if !self.opts.auto_merge {
                        return Ok("ready".into());
                    }
                    match self.merge(cs_key).await {
                        Ok(()) => return Ok("merged".into()),
                        // tests went stale (e.g. another merge changed a dependency): re-test
                        // instead of dead-ending in `ready` (review M6 #9)
                        Err(AgentError::Store(kodehub_store::StoreError::Conflict(m)))
                            if m.contains("tests_ok=false") =>
                        {
                            self.to(
                                cs_key,
                                ChangesetState::Testing,
                                "orchestrator",
                                "tests stale at merge time — re-testing",
                            )
                            .await?;
                        }
                        Err(e) => return Err(e),
                    }
                }
                ChangesetState::Merged => return Ok("merged".into()),
                ChangesetState::Rejected => return Ok("rejected".into()),
                ChangesetState::Escalated => return Ok("escalated".into()),
            }
        }
        Err(AgentError::Workflow("workflow did not converge".into()))
    }

    /// The coder touched symbols the human did not approve: add them to the plan (flagged) and
    /// send the changeset back for re-approval (review M6 #13).
    async fn request_reapproval(&self, cs_key: &str, out_of_plan: &[String]) -> Result<String> {
        let cs = self.store.get_changeset(cs_key).await?;
        let mut plan = cs["plan"].clone();
        if let Some(ch) = plan["changes"].as_array_mut() {
            for k in out_of_plan {
                let p = &cs["proposals"][k];
                ch.push(json!({"symbol": if p["new"] == json!(true) { Value::Null } else { json!(k) },
                               "display_name": p["display_name"], "kind": p["kind"], "language": p["language"],
                               "action": if p["new"] == json!(true) { "add" } else { "modify" },
                               "component": p["component"], "description": "added by the coder outside the approved plan",
                               "out_of_plan": true}));
            }
        }
        self.store
            .patch_changeset(cs_key, json!({"plan": plan, "skip_to_review": true}))
            .await?;
        self.to(
            cs_key,
            ChangesetState::Planned,
            "orchestrator",
            "coder changed symbols outside the approved plan — re-approval needed",
        )
        .await?;
        self.activity(
            cs_key,
            "orchestrator",
            &format!(
                "Out-of-plan changes need your approval: {}",
                out_of_plan.join(", ")
            ),
        )
        .await;
        Ok("awaiting re-approval".into())
    }

    async fn escalate(&self, cs_key: &str, why: &str) -> Result<String> {
        let cs = self.store.get_changeset(cs_key).await?;
        let d = self
            .store
            .create_decision(
                json!({"title": format!("Escalated: {}", cs["title"].as_str().unwrap_or(cs_key)),
                       "choice": "Escalate to the human — agents could not reach consensus.",
                       "rationale": why, "made_by": "orchestrator", "changeset": cs_key}),
                &[],
            )
            .await?;
        let mut decisions = cs["decisions"].as_array().cloned().unwrap_or_default();
        decisions.push(d["_id"].clone());
        self.store
            .patch_changeset(cs_key, json!({"decisions": decisions}))
            .await?;
        self.to(cs_key, ChangesetState::Escalated, "orchestrator", why)
            .await?;
        self.activity(
            cs_key,
            "orchestrator",
            &format!("Escalated to human: {why}"),
        )
        .await;
        Ok("escalated".into())
    }

    /// Tester: tests from requirements and signatures only (never function bodies).
    async fn tester(&self, cs_key: &str) -> Result<()> {
        let cs = self.store.get_changeset(cs_key).await?;
        let reqs = str_list(&cs["requirements"]);
        let g = self.store.load_code_graph().await?;
        let relevant = relevant_symbols(&self.store, &g, &reqs).await?;
        let input = format!(
            "## Mission\n{}\n\n## Requirements\n{}\n## Approved plan\n{}\n\n## Public signatures and data shapes (function bodies deliberately hidden)\n{}\n## Task\nWrite the tests the requirements call for that do not exist yet (especially for anything that changed). Return JSON `tests` (each: display_name, language, body, verifies [requirement keys], tests_for [symbol keys], description) and `notes`.",
            mission(&self.store).await?,
            requirements_md(&self.store, &reqs, true).await?,
            serde_json::to_string_pretty(&cs["plan"]).unwrap_or_default(),
            code_md(&g, &relevant, false, true),
        );
        let out = self
            .call(
                Role::Tester,
                "write tests",
                input,
                schemas::tests(),
                Some(cs_key),
            )
            .await?;
        for t in out.json["tests"].as_array().cloned().unwrap_or_default() {
            let lang: Language =
                serde_json::from_value(t["language"].clone()).unwrap_or(Language::Rust);
            let tests_for: Vec<String> = str_list(&t["tests_for"])
                .into_iter()
                .map(|k| key_of(&k).to_string())
                .filter(|k| g.symbols.contains_key(k))
                .collect();
            let component = self.component_of(tests_for.first()).await?;
            self.store
                .propose_version(
                    cs_key,
                    &ProposeVersion {
                        symbol: None,
                        display_name: Some(
                            t["display_name"]
                                .as_str()
                                .unwrap_or("test_unnamed")
                                .to_string(),
                        ),
                        kind: Some(Kind::Test),
                        language: Some(lang),
                        visibility: None,
                        signature: String::new(),
                        body: t["body"].as_str().unwrap_or("").to_string(),
                        attributes: None,
                        component,
                        member_of: None,
                        verifies: str_list(&t["verifies"])
                            .into_iter()
                            .map(|k| key_of(&k).to_string())
                            .collect(),
                        tests_for,
                        description: t["description"].as_str().map(String::from),
                        by: Some("tester".into()),
                    },
                )
                .await?;
        }
        self.activity(
            cs_key,
            "tester",
            &format!(
                "Proposed {} test(s) from the requirements",
                out.json["tests"].as_array().map(|a| a.len()).unwrap_or(0)
            ),
        )
        .await;
        Ok(())
    }

    async fn component_of(&self, sym: Option<&String>) -> Result<Option<String>> {
        let Some(s) = sym else { return Ok(None) };
        let rows = self
            .store
            .aql("FOR e IN @@contains FILTER e._to == CONCAT(@pfx, 'symbols/', @s) AND STARTS_WITH(e._from, CONCAT(@pfx, 'components/')) RETURN PARSE_IDENTIFIER(e._from).key", json!({"s": s}))
            .await?;
        Ok(rows.first().and_then(Value::as_str).map(String::from))
    }

    /// Coder: initial implementation or rework for findings / failing tests.
    /// Returns symbols touched outside the approved plan (initial implementation only).
    async fn coder(&self, cs_key: &str, findings: &[Value], failures: &str) -> Result<Vec<String>> {
        let cs = self.store.get_changeset(cs_key).await?;
        let reqs = str_list(&cs["requirements"]);
        let g = self.store.load_code_graph().await?;
        let mut relevant = relevant_symbols(&self.store, &g, &reqs).await?;
        for c in cs["plan"]["changes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            if let Some(s) = c["symbol"].as_str() {
                relevant.insert(s.to_string());
            }
        }
        let proposals = cs["proposals"].clone();
        let current_props: String = proposals
            .as_object()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|(k, p)| {
                format!(
                    "#### `{}` (key `{k}`, {} {}, {})\nsignature: `{}`\n```{}\n{}\n```\n",
                    p["display_name"].as_str().unwrap_or(k),
                    p["language"].as_str().unwrap_or(""),
                    p["kind"].as_str().unwrap_or(""),
                    if p["new"] == json!(true) {
                        "new"
                    } else {
                        "modified"
                    },
                    p["signature"].as_str().unwrap_or(""),
                    p["language"].as_str().unwrap_or(""),
                    p["body"].as_str().unwrap_or("")
                )
            })
            .collect();
        let mut task = String::from("Implement the approved plan. Propose a complete new version for every symbol you add or change (not for tests).");
        if !findings.is_empty() {
            task = format!(
                "The blind reviewer raised these findings. Fix them in code (propose updated versions) or dispute them; respond to every finding id.\n{}",
                findings.iter().map(|f| format!("- `{}` [{}] {} — {}", f["id"].as_str().unwrap_or(""), f["severity"].as_str().unwrap_or(""), f["location"].as_str().unwrap_or(""), f["finding"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n")
            );
        }
        if !failures.is_empty() {
            task = format!("These tests fail against your proposals. Fix the code (tests come from the requirements; only dispute a test if it contradicts a requirement).\n```\n{failures}\n```");
        }
        let input = format!(
            "## Mission\n{}\n\n## Requirements\n{}\n## Decisions\n{}\n\n## Approved plan\n{}\n\n## Current code\n{}\n## Proposals in this changeset so far (tests included)\n{}\n## Task\n{}\n\nReturn JSON: `changes` (each: symbol = existing key or \"\" for new, display_name, kind, language, signature, body, attributes_json = JSON object as a string or \"\" to keep existing attributes, component, member_of, description), `responses` (finding_id, action fixed|disputed, explanation) and `notes`.",
            mission(&self.store).await?,
            requirements_md(&self.store, &reqs, true).await?,
            self.decisions_md(&relevant).await?,
            serde_json::to_string_pretty(&cs["plan"]).unwrap_or_default(),
            code_md(&g, &relevant, true, false),
            if current_props.is_empty() { "(none)".into() } else { current_props },
            task,
        );
        let label = if findings.is_empty() && failures.is_empty() {
            "implement plan".to_string()
        } else {
            format!("rework round {}", cs["rounds"])
        };
        let out = self
            .call(Role::Coder, &label, input, schemas::code(), Some(cs_key))
            .await?;
        let mut n = 0;
        let mut refused: Vec<String> = Vec::new();
        let mut out_of_plan: Vec<String> = Vec::new();
        let initial = findings.is_empty() && failures.is_empty();
        let plan_syms: BTreeSet<String> = cs["plan"]["changes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .flat_map(|c| {
                [
                    c["symbol"].as_str().map(String::from),
                    c["display_name"].as_str().map(String::from),
                ]
            })
            .flatten()
            .collect();
        for c in out.json["changes"].as_array().cloned().unwrap_or_default() {
            let sym = c["symbol"]
                .as_str()
                .map(key_of)
                .filter(|s| !s.is_empty() && g.symbols.contains_key(*s))
                .map(String::from);
            // tests belong to the tester (written from requirements); the coder may not change
            // them (review M6 #5)
            // resolve by name too: `symbol: ""` + an existing display name is a modification
            let lang_hint: Option<Language> = serde_json::from_value(c["language"].clone()).ok();
            let by_name = c["display_name"].as_str().and_then(|n| {
                g.symbols.values().find(|s| {
                    s.display_name == n && lang_hint.map(|l| l == s.language).unwrap_or(true)
                })
            });
            let sym = sym.or_else(|| by_name.map(|s| s.id.clone()));
            let targets_test = c["kind"] == json!("test")
                || sym
                    .as_ref()
                    .and_then(|k| g.symbols.get(k))
                    .map(|s| s.kind == Kind::Test)
                    .unwrap_or(false);
            if targets_test {
                refused.push(c["display_name"].as_str().unwrap_or("?").to_string());
                continue;
            }
            let name = c["display_name"].as_str().unwrap_or("").to_string();
            let in_plan = sym.as_ref().map(|k| plan_syms.contains(k)).unwrap_or(false)
                || plan_syms.contains(&name);
            let attrs_s = c["attributes_json"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string();
            let attributes = if attrs_s.is_empty() {
                // keep existing attributes (or the ones already proposed)
                sym.as_ref().and_then(|k| {
                    proposals
                        .get(k)
                        .map(|p| p["attributes"].clone())
                        .or_else(|| {
                            g.symbols
                                .get(k)
                                .and_then(|s| g.version(&s.version).ok())
                                .map(|v| v.attributes.clone())
                        })
                })
            } else {
                Some(serde_json::from_str::<Value>(&attrs_s).map_err(|e| {
                    AgentError::Output(format!("attributes_json for {}: {e}", c["display_name"]))
                })?)
            };
            let kind: Option<Kind> = serde_json::from_value(c["kind"].clone()).ok();
            let lang: Option<Language> = serde_json::from_value(c["language"].clone()).ok();
            let member_of = c["member_of"]
                .as_str()
                .map(key_of)
                .filter(|s| !s.is_empty())
                .map(String::from);
            self.store
                .propose_version(
                    cs_key,
                    &ProposeVersion {
                        symbol: sym.clone(),
                        display_name: c["display_name"].as_str().map(String::from),
                        kind,
                        language: lang,
                        visibility: None,
                        signature: c["signature"].as_str().unwrap_or("").to_string(),
                        body: c["body"].as_str().unwrap_or("").to_string(),
                        attributes,
                        component: c["component"]
                            .as_str()
                            .map(key_of)
                            .filter(|s| !s.is_empty())
                            .map(String::from),
                        member_of,
                        verifies: vec![],
                        tests_for: vec![],
                        description: c["description"].as_str().map(String::from),
                        by: Some("coder".into()),
                    },
                )
                .await?;
            n += 1;
            // round 0: anything outside the plan needs re-approval; later rounds: modifying an
            // existing symbol outside the plan does (new helpers are flagged, final review #11)
            let modifies_existing = sym
                .as_ref()
                .map(|k| g.symbols.contains_key(k))
                .unwrap_or(false);
            if !in_plan && (initial || modifies_existing) {
                let p = self.store.get_changeset(cs_key).await?;
                let key = p["proposals"]
                    .as_object()
                    .and_then(|o| {
                        o.iter()
                            .find(|(_, v)| v["display_name"] == json!(name))
                            .map(|(k, _)| k.clone())
                    })
                    .unwrap_or(name.clone());
                out_of_plan.push(key);
            }
        }
        if !refused.is_empty() {
            self.activity(
                cs_key,
                "orchestrator",
                &format!(
                    "Refused coder changes to tests (tests are the tester's): {}",
                    refused.join(", ")
                ),
            )
            .await;
        }
        // Coder responses are kept for the Decision log — never shown to the reviewer.
        let mut responses = cs["coder_responses"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        for r in out.json["responses"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            if let Some(id) = r["finding_id"].as_str() {
                responses.insert(id.to_string(), r.clone());
            }
        }
        self.store
            .patch_changeset(cs_key, json!({"coder_responses": responses}))
            .await?;
        self.activity(
            cs_key,
            "coder",
            &format!("Proposed {n} version(s) ({label})"),
        )
        .await;
        Ok(out_of_plan)
    }

    /// Blind review. Input = mission + requirements + diff + tests + the reviewer's own previous
    /// findings. Returns the findings that still require rework.
    async fn reviewer(&self, cs_key: &str, round: i64) -> Result<Vec<Value>> {
        let cs = self.store.get_changeset(cs_key).await?;
        let reqs = str_list(&cs["requirements"]);
        let g = self.store.load_code_graph().await?;
        let verifying: BTreeSet<String> = self
            .store
            .aql(
                "FOR e IN @@verifies FILTER e._to IN @r RETURN PARSE_IDENTIFIER(e._from).key",
                json!({"r": reqs.iter().map(|r| format!("requirements/{r}")).collect::<Vec<_>>()}),
            )
            .await?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let previous: Vec<Value> = cs["reviews"]
            .as_array()
            .and_then(|r| r.last())
            .and_then(|r| r["findings"].as_array().cloned())
            .unwrap_or_default();
        let prev_md = if previous.is_empty() {
            "(first review)".to_string()
        } else {
            previous
                .iter()
                .map(|f| {
                    format!(
                        "- id `{}` [{}] {} — {} (status was: {})",
                        f["id"].as_str().unwrap_or(""),
                        f["severity"].as_str().unwrap_or(""),
                        f["location"].as_str().unwrap_or(""),
                        f["finding"].as_str().unwrap_or(""),
                        f["status"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let input = format!(
            "## Mission\n{}\n\n## Requirements\n{}\n## Diff (proposed versions)\n{}\n## Tests\n{}\n## Your findings from the previous round\n{}\n\n## Task\nReview round {}. Return JSON `verdict`, `summary`, `findings` (id = previous id or \"\" for new, severity, location, finding, status).",
            mission(&self.store).await?,
            requirements_md(&self.store, &reqs, false).await?,
            diff_md(&g, &cs["proposals"]),
            tests_md(&g, &cs["proposals"], &{
                // every test the change affects, not only those verifying its requirements
                let mut all = verifying.clone();
                if let Ok((og, touched)) = self.store.overlay_graph(cs_key).await {
                    if let Ok(a) = self.store.affected_tests(cs_key, &og, &touched).await {
                        all.extend(a.into_iter().filter(|t| g.symbols.contains_key(t)));
                    }
                }
                all
            }),
            prev_md,
            round + 1,
        );
        let out = self
            .call(
                Role::Reviewer,
                &format!("blind review round {}", round + 1),
                input,
                schemas::review(),
                Some(cs_key),
            )
            .await?;
        // merge: previous findings keep their ids; anything not mentioned stays as it was
        let mut merged: BTreeMap<String, Value> = BTreeMap::new();
        for f in &previous {
            if let Some(id) = f["id"].as_str() {
                merged.insert(id.to_string(), f.clone());
            }
        }
        let mut new_findings = Vec::new();
        for f in out.json["findings"].as_array().cloned().unwrap_or_default() {
            let id = f["id"].as_str().unwrap_or("").to_string();
            if !id.is_empty() && merged.contains_key(&id) {
                let e = merged.get_mut(&id).unwrap();
                e["status"] = f["status"].clone();
                if f["status"] == "resolved" {
                    e["resolution"] = json!(format!("resolved in review round {}", round + 1));
                }
            } else {
                let mut nf = f.clone();
                nf["id"] = json!("");
                nf["status"] = json!("open");
                new_findings.push(nf);
            }
        }
        let mut all: Vec<Value> = merged.into_values().collect();
        all.extend(new_findings);
        let review = self
            .store
            .submit_review(
                cs_key,
                &all,
                out.json["summary"].as_str().unwrap_or(""),
                "reviewer",
                "engine_reviewer",
            )
            .await?;
        let open: Vec<Value> = review["findings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|f| {
                f["status"] == "open" && (f["severity"] == "blocking" || f["severity"] == "major")
            })
            .collect();
        self.activity(
            cs_key,
            "reviewer",
            &format!(
                "Blind review round {}: {} finding(s), {} need rework",
                round + 1,
                review["findings"].as_array().map(|a| a.len()).unwrap_or(0),
                open.len()
            ),
        )
        .await;
        Ok(open)
    }

    /// Run affected tests on the proposed closure. Returns (all passed, failure report).
    async fn run_tests(&self, cs_key: &str) -> Result<(bool, String)> {
        let (g, touched) = self.store.overlay_graph(cs_key).await?;
        let affected = self.store.affected_tests(cs_key, &g, &touched).await?;
        self.set_status(Role::Tester, "working", "run affected tests", Some(cs_key));
        let outcomes = self
            .runner
            .run_tests(&self.store, &g, &affected, false, Some(cs_key), None)
            .await;
        self.set_status(Role::Tester, "idle", "run affected tests", Some(cs_key));
        let outcomes = outcomes?;
        for o in &outcomes {
            (self.emit)(
                "test.result",
                json!({"test": o.test, "name": o.name, "status": o.status, "changeset": cs_key, "cached": o.cached}),
            );
        }
        self.store
            .patch_changeset(cs_key, json!({"test_results": outcomes}))
            .await?;
        let failed: Vec<_> = outcomes.iter().filter(|o| o.status != "pass").collect();
        let report = failed
            .iter()
            .map(|o| {
                format!(
                    "--- {} ({})\n{}",
                    o.name,
                    o.language.as_str(),
                    o.output.chars().take(3000).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        self.activity(
            cs_key,
            "tester",
            &format!(
                "{} of {} affected test(s) pass",
                outcomes.len() - failed.len(),
                outcomes.len()
            ),
        )
        .await;
        Ok((failed.is_empty() && !outcomes.is_empty(), report))
    }

    /// Record a Decision for every blocking/major finding that was resolved.
    async fn record_decisions(&self, cs_key: &str) -> Result<()> {
        let cs = self.store.get_changeset(cs_key).await?;
        let responses = cs["coder_responses"].clone();
        let mut seen: BTreeMap<String, Value> = BTreeMap::new();
        for r in cs["reviews"].as_array().cloned().unwrap_or_default() {
            for f in r["findings"].as_array().cloned().unwrap_or_default() {
                if let Some(id) = f["id"].as_str() {
                    seen.insert(id.to_string(), f.clone());
                }
            }
        }
        let touched: Vec<String> = cs["proposals"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(_, p)| p["kind"] != "test" && p["new"] != json!(true))
                    .map(|(k, _)| format!("symbols/{k}"))
                    .collect()
            })
            .unwrap_or_default();
        let mut decisions = cs["decisions"].as_array().cloned().unwrap_or_default();
        for (id, f) in seen {
            let sev = f["severity"].as_str().unwrap_or("");
            if !(sev == "blocking" || sev == "major") || f["status"] != "resolved" {
                continue;
            }
            let resp = &responses[&id];
            let location = f["location"].as_str().unwrap_or("");
            let justifies: Vec<String> = touched
                .iter()
                .filter(|t| {
                    location.is_empty()
                        || location.contains(t.trim_start_matches("symbols/"))
                        || cs["proposals"][t.trim_start_matches("symbols/")]["display_name"]
                            .as_str()
                            .map(|n| location.contains(n))
                            .unwrap_or(false)
                })
                .cloned()
                .collect();
            let justifies = if justifies.is_empty() {
                touched.clone()
            } else {
                justifies
            };
            let d = self
                .store
                .create_decision(
                    json!({
                        "title": format!("Review: {}", truncate(f["finding"].as_str().unwrap_or(""), 90)),
                        "choice": resp["explanation"].as_str().unwrap_or("Fixed in code during rework."),
                        "rationale": format!("Blind reviewer ({sev}) at {location}: {}. Coder {}: {}. Reviewer confirmed it resolved.",
                            f["finding"].as_str().unwrap_or(""), resp["action"].as_str().unwrap_or("fixed"), resp["explanation"].as_str().unwrap_or("see changeset")),
                        "alternatives": [f["finding"]],
                        "made_by": "agents:coder+reviewer",
                        "changeset": cs_key,
                        "finding": id,
                    }),
                    &justifies,
                )
                .await?;
            decisions.push(d["_id"].clone());
            self.activity(
                cs_key,
                "orchestrator",
                &format!("Decision recorded: {}", d["title"].as_str().unwrap_or("")),
            )
            .await;
        }
        self.store
            .patch_changeset(cs_key, json!({"decisions": decisions}))
            .await?;
        Ok(())
    }

    pub async fn merge(&self, cs_key: &str) -> Result<()> {
        self.store
            .merge_changeset(cs_key, "orchestrator", false)
            .await?;
        let cs = self.store.get_changeset(cs_key).await?;
        (self.emit)(
            "changeset.updated",
            json!({"key": cs_key, "state": "merged"}),
        );
        (self.emit)(
            "node.changed",
            json!({"change": "merged", "changeset": cs_key}),
        );
        self.activity(
            cs_key,
            "orchestrator",
            "Merged — all current_version edges moved in one transaction",
        )
        .await;
        if self.opts.warm_cache {
            let touched: Vec<String> = cs["merged_versions"]
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            self.warm(&touched).await;
        }
        Ok(())
    }

    /// Pre-build run-now harnesses for touched runnable functions and their dependents.
    pub async fn warm(&self, touched: &[String]) {
        let Ok(g) = self.store.load_code_graph().await else {
            return;
        };
        let mut targets: BTreeSet<String> = BTreeSet::new();
        for t in touched {
            targets.insert(t.clone());
            targets.extend(g.dependents(t));
        }
        for t in targets {
            if kodehub_render::runnable(&g, &t).is_ok() {
                let lang = g.symbols[&t].language;
                if let Ok(b) = self
                    .runner
                    .build(
                        &g,
                        &BuildTarget::Run {
                            language: lang,
                            symbol: t.clone(),
                        },
                    )
                    .await
                {
                    if !b.cache_hit {
                        let _ = self.runner.record_artifact(&self.store, &b).await;
                    }
                }
            }
        }
        (self.emit)(
            "agent.activity",
            json!({"role": "orchestrator", "message": "Build cache warmed"}),
        );
    }

    // ------------------------------------------------------------------ chat

    pub async fn chat(&self, message: &str, references: &[String]) -> Result<Value> {
        let reqs = self.store.coverage(None).await?;
        let req_list: String = reqs
            .requirements
            .iter()
            .map(|r| format!("- `{}` {} — {} [{}]", r.key, r.title, r.statement, r.status))
            .collect::<Vec<_>>()
            .join("\n");
        let mut refs = String::new();
        for r in references {
            if let Ok(n) = self.store.get_node(r).await {
                refs.push_str(&format!("- `{r}`: {}\n", truncate(&n.to_string(), 1500)));
            }
        }
        let history = self
            .store
            .aql(
                "FOR m IN @@chat_messages SORT m.created_at DESC LIMIT 8 RETURN m",
                json!({}),
            )
            .await?;
        let hist: String = history
            .iter()
            .rev()
            .map(|m| {
                format!(
                    "{}: {}",
                    m["role"].as_str().unwrap_or(""),
                    m["text"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let input = format!(
            "## Mission\n{}\n\n## Requirements\n{}\n\n## Referenced nodes\n{}\n\n## Recent conversation\n{}\n\n## Human says\n{}\n\n## Task\nReply briefly. If the human asks for a behaviour change, choose an action: `add_edge_case` / `add_example` (set `requirement` key, `description`, `input_json`, `expected_json`), `create_requirement` (`title`, `statement`), or `open_changeset` (`title`, `description` as intent, `requirement` optional). Otherwise action `none`. Unused fields are \"\".",
            mission(&self.store).await?, req_list, if refs.is_empty() { "(none)".into() } else { refs }, hist, message
        );
        let now = now_iso();
        self.store.db.insert("chat_messages", &json!({"role": "human", "text": message, "references": references, "created_at": now}), None).await?;
        let out = self
            .call(Role::Orchestrator, "chat", input, schemas::chat(), None)
            .await?;
        let a = out.json.clone();
        // Requirement changes proposed in chat wait for a human click (review M6 #19): the
        // orchestrator never edits requirements on its own.
        let proposal = if a["action"].as_str().unwrap_or("none") != "none" {
            a.clone()
        } else {
            Value::Null
        };
        let reply = json!({"role": "orchestrator", "text": a["reply"], "action": a["action"], "proposal": proposal,
                           "status": if proposal.is_null() { "none" } else { "pending" }, "created_at": now_iso()});
        let saved = self.store.db.insert("chat_messages", &reply, None).await?;
        (self.emit)(
            "agent.activity",
            json!({"role": "orchestrator", "message": "chat reply", "chat": true}),
        );
        Ok(saved)
    }

    /// Apply (or discard) an action the orchestrator proposed in chat. Only a human calls this.
    pub async fn apply_chat_action(&self, message_key: &str, apply: bool) -> Result<Value> {
        let msg = self
            .store
            .db
            .get("chat_messages", message_key, None)
            .await?;
        if msg["status"] != json!("pending") {
            return Err(AgentError::Workflow(
                "this proposal was already handled".into(),
            ));
        }
        if !apply {
            return Ok(self
                .store
                .db
                .patch(
                    "chat_messages",
                    message_key,
                    &json!({"status": "discarded"}),
                    None,
                )
                .await?);
        }
        let a = msg["proposal"].clone();
        let parse = |s: &str| -> Value {
            serde_json::from_str(s).unwrap_or_else(|_| {
                if s.is_empty() {
                    Value::Null
                } else {
                    json!(s)
                }
            })
        };
        let mut changeset: Option<String> = None;
        match a["action"].as_str().unwrap_or("none") {
            "add_edge_case" | "add_example" => {
                let req = key_of(a["requirement"].as_str().unwrap_or("")).to_string();
                let field = if a["action"] == "add_edge_case" {
                    "edge_cases"
                } else {
                    "examples"
                };
                let cur = self.store.db.get("requirements", &req, None).await?;
                let mut list = cur[field].as_array().cloned().unwrap_or_default();
                list.push(json!({"description": a["description"], "input": parse(a["input_json"].as_str().unwrap_or("")), "expected": parse(a["expected_json"].as_str().unwrap_or(""))}));
                let (_, changed) = self
                    .store
                    .update_requirement(
                        &req,
                        &json!({field: list}),
                        "human (proposed by orchestrator)",
                        true,
                    )
                    .await?;
                if changed {
                    (self.emit)(
                        "node.changed",
                        json!({"id": format!("requirements/{req}"), "change": "requirement_edited"}),
                    );
                    changeset = Some(self.on_requirement_edited(&req, "human").await?);
                }
            }
            "create_requirement" => {
                let r = self
                    .store
                    .create_requirement(json!({"title": a["title"], "statement": a["statement"]}))
                    .await?;
                (self.emit)("node.changed", json!({"id": r["_id"], "change": "created"}));
            }
            "open_changeset" => {
                let req: Vec<String> = a["requirement"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| vec![key_of(s).to_string()])
                    .unwrap_or_default();
                let cs = self
                    .store
                    .open_changeset(
                        a["title"].as_str().unwrap_or("Change"),
                        a["description"].as_str().unwrap_or(""),
                        &req,
                        "orchestrator",
                    )
                    .await?;
                let k = cs["_key"].as_str().unwrap_or_default().to_string();
                self.plan(&k).await?;
                changeset = Some(k);
            }
            _ => {}
        }
        Ok(self
            .store
            .db
            .patch(
                "chat_messages",
                message_key,
                &json!({"status": "applied", "changeset": changeset}),
                None,
            )
            .await?)
    }
}

fn str_list(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}
