//! The environment agent (doc 06, D-059, D-060): deploys project versions through gates.
//! Gates are deterministic checks (decisions.md D-027), recorded on the deployment and in its
//! deploy logs.
//!
//! - → test:    the project version builds for every target (Rust lib + Python package).
//! - → qa:      every test passes; no requirement fails or is unverified; no orphans; the tests
//!   of live use cases are in the project version.
//! - → preprod: smoke run (every test re-executed).
//! - → prod:    smoke run, and a human approved the deployment.
//!
//! Promotion is ordered (one environment up at a time); hotfixes and rollbacks deploy straight
//! into the environment they fix and run its gate. Each deployer the deployment uses performs
//! its part (v0.1: `k8s_namespace`, a simulated namespace on this machine) and logs it.

use std::collections::BTreeMap;

use kodehub_core::model::{now_iso, Kind, Language};
use kodehub_core::CodeGraph;
use kodehub_runner::{BuildTarget, Runner};
use kodehub_store::deploy::EnvDef;
use kodehub_store::Store;
use serde_json::{json, Value};

use crate::{AgentError, Result};

pub struct EnvironmentAgent<'a> {
    pub store: &'a Store,
    pub runner: &'a Runner,
}

fn roots(g: &CodeGraph, lang: Language) -> Vec<String> {
    g.symbols
        .values()
        .filter(|s| s.language == lang && s.kind != Kind::Test)
        .map(|s| s.id.clone())
        .collect()
}

impl EnvironmentAgent<'_> {
    async fn build_gate(&self, g: &CodeGraph) -> Result<Value> {
        let mut checks = Vec::new();
        let mut ok = true;
        for lang in [Language::Rust, Language::Python] {
            let r = roots(g, lang);
            if r.is_empty() {
                continue;
            }
            // namespaces (projection layer) map to pub mod / sub-module re-exports
            let namespaces: Vec<kodehub_render::NamespaceSpec> = self
                .store
                .namespaces()
                .await?
                .into_iter()
                .map(|(name, exposes)| kodehub_render::NamespaceSpec { name, exposes })
                .collect();
            let b = self
                .runner
                .build(
                    g,
                    &BuildTarget::Lib {
                        language: lang,
                        roots: r.clone(),
                        namespaces,
                    },
                )
                .await?;
            self.runner.record_artifact(self.store, &b, g, &r).await?;
            ok &= b.ok;
            checks.push(json!({"check": format!("build {}", b.target), "ok": b.ok, "cache_hit": b.cache_hit,
                               "detail": if b.ok { String::new() } else { b.log.chars().take(2000).collect() }}));
        }
        Ok(json!({"ok": ok, "checks": checks}))
    }

    /// `force` re-executes every test (the smoke run) instead of trusting cached passes.
    async fn tests_gate(&self, g: &CodeGraph, label: &str, force: bool) -> Result<(bool, Value)> {
        let tests: Vec<String> = g
            .symbols
            .values()
            .filter(|s| s.kind == Kind::Test)
            .map(|s| s.id.clone())
            .collect();
        let out = self
            .runner
            .run_tests(self.store, g, &tests, force, None, None)
            .await?;
        let failed: Vec<String> = out
            .iter()
            .filter(|o| o.status != "pass")
            .map(|o| o.name.clone())
            .collect();
        Ok((
            failed.is_empty(),
            json!({"check": label, "ok": failed.is_empty(), "total": out.len(), "failed": failed}),
        ))
    }

    /// Gate kind of an environment, by name (custom environments get the tests gate).
    fn gate_kind(env: &EnvDef) -> &'static str {
        match env.name.as_str() {
            "test" => "build",
            "qa" => "tests",
            "preprod" => "smoke",
            "prod" => "approval",
            _ if env.order == 0 => "none",
            _ if env.requires_human_approval => "approval",
            _ => "tests",
        }
    }

    /// The checks for project version `pv` entering `env` (spec §16.3). `approved`: a human
    /// approved this deployment.
    pub async fn gate_checks(&self, pv: &str, env: &str, approved: bool) -> Result<Value> {
        let envs = self.store.env_defs().await?;
        let def = envs
            .iter()
            .find(|e| e.name == env)
            .cloned()
            .ok_or_else(|| AgentError::Workflow(format!("unknown environment {env}")))?;
        let mut checks = Vec::new();
        let g = self.store.load_code_graph_at(Some(pv)).await?;
        match Self::gate_kind(&def) {
            "build" => {
                let b = self.build_gate(&g).await?;
                checks.extend(b["checks"].as_array().cloned().unwrap_or_default());
            }
            "tests" => {
                let (_, t) = self.tests_gate(&g, "all tests pass", false).await?;
                checks.push(t);
                // evaluated on the project version itself, not the working tree
                let rep = self.store.project_version_trace(pv).await?;
                let mut risky = rep["unverified_requirements"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                risky.extend(
                    rep["at_risk_requirements"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                );
                checks.push(json!({"check": "no requirement at risk or unverified", "ok": risky.is_empty(), "requirements": risky}));
                let failing = rep["failing_requirements"].clone();
                checks.push(json!({"check": "no failing requirements", "ok": failing.as_array().map(|a| a.is_empty()).unwrap_or(false), "failing": failing}));
                let orphans = rep["orphans"].clone();
                checks.push(json!({"check": "no orphans", "ok": orphans.as_array().map(|a| a.is_empty()).unwrap_or(false), "orphans": orphans}));
                // live use cases' tests must be in the project version (spec §16.3)
                let missing = self.live_use_case_tests_missing(&g).await?;
                checks.push(json!({"check": "live use cases are covered", "ok": missing.is_empty(), "missing": missing}));
            }
            "smoke" => {
                let (_, t) = self.tests_gate(&g, "smoke run (re-executed)", true).await?;
                checks.push(t);
            }
            "approval" => {
                let (_, t) = self.tests_gate(&g, "smoke run (re-executed)", true).await?;
                checks.push(t);
                checks.push(json!({"check": "human approval", "ok": approved}));
            }
            _ => {}
        }
        let ok = checks.iter().all(|c| c["ok"] == json!(true));
        Ok(
            json!({"ok": ok, "environment": env, "project_version": pv, "checks": checks, "checked_at": now_iso()}),
        )
    }

    async fn live_use_case_tests_missing(&self, g: &CodeGraph) -> Result<Vec<String>> {
        let rows = self
            .store
            .aql(
                "FOR u IN @@use_cases FILTER u.state == 'live'
                   FOR e IN @@verified_by FILTER e._from == u._id RETURN PARSE_IDENTIFIER(e._to).key",
                json!({}),
            )
            .await?;
        Ok(rows
            .iter()
            .filter_map(Value::as_str)
            .filter(|k| !g.symbols.contains_key(*k))
            .map(str::to_string)
            .collect())
    }

    /// Create a deployment and run it unless it waits for a human (production).
    pub async fn deploy(
        &self,
        pv: &str,
        env: &str,
        by: &str,
        reason: &str,
        ordered: bool,
    ) -> Result<Value> {
        let d = self
            .store
            .create_deployment(pv, env, by, reason, ordered)
            .await?;
        let key = d["_key"].as_str().unwrap_or_default().to_string();
        if d["state"] == "awaiting_approval" {
            return Ok(
                json!({"deployed": false, "awaiting_approval": true, "deployment": self.store.get_deployment(&key).await?}),
            );
        }
        self.run(&key, by).await
    }

    /// A human approves a deployment waiting for approval; it then runs.
    pub async fn approve_and_run(&self, key: &str, by: &str) -> Result<Value> {
        self.store.approve_deployment(key, by).await?;
        self.run(key, by).await
    }

    /// One attempt of a deployment: the environment's gate, then each deployer's part.
    pub async fn run(&self, key: &str, by: &str) -> Result<Value> {
        self.store.start_deployment(key).await?;
        let d = self.store.get_deployment(key).await?;
        let pv = d["project_version"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let env = d["environment"].as_str().unwrap_or_default().to_string();
        let approved = !d["approved_by"].is_null();
        let gate = match self.gate_checks(&pv, &env, approved).await {
            Ok(g) => g,
            Err(e) => {
                let gate = json!({"ok": false, "error": e.to_string()});
                let out = self.store.finish_deployment(key, false, gate, by).await?;
                return Ok(json!({"deployed": false, "deployment": out}));
            }
        };
        self.store
            .deploy_log(
                key,
                "gate",
                if gate["ok"] == true { "ok" } else { "failed" },
                gate.clone(),
            )
            .await?;
        if gate["ok"] != json!(true) {
            let out = self
                .store
                .finish_deployment(key, false, gate.clone(), by)
                .await?;
            return Ok(json!({"deployed": false, "gate": gate, "deployment": out}));
        }
        for dep in d["deployers"].as_array().cloned().unwrap_or_default() {
            let detail = self.run_deployer(&dep, &env, &pv)?;
            self.store
                .deploy_log(
                    key,
                    &format!("deployer {}", dep["name"].as_str().unwrap_or("")),
                    "ok",
                    detail,
                )
                .await?;
        }
        let out = self
            .store
            .finish_deployment(key, true, gate.clone(), by)
            .await?;
        Ok(json!({"deployed": true, "gate": gate, "deployment": out}))
    }

    /// Perform one deployer's part. `k8s_namespace` (v0.1): the environment's simulated
    /// namespace — a directory and config file per environment and target on this machine.
    fn run_deployer(&self, dep: &Value, env: &str, pv: &str) -> Result<Value> {
        let kind = dep["kind"].as_str().unwrap_or("k8s_namespace");
        if kind != "k8s_namespace" {
            return Err(AgentError::Workflow(format!(
                "deployer kind `{kind}` is not available in this build"
            )));
        }
        let target = dep["target"].as_str().unwrap_or("default");
        let dir = self.runner.home.join("envs").join(env).join(target);
        std::fs::create_dir_all(&dir).map_err(|e| AgentError::Workflow(e.to_string()))?;
        let cfg = format!(
            "# simulated {env} environment, target {target}\nenvironment = \"{env}\"\nnamespace = \"{}\"\nproject_version = \"{pv}\"\ndeployed_at = \"{}\"\nstubs_allowed = {}\n",
            dep["namespace"].as_str().unwrap_or(""),
            now_iso(),
            env == "dev"
        );
        std::fs::write(dir.join("config.toml"), cfg)
            .map_err(|e| AgentError::Workflow(e.to_string()))?;
        Ok(
            json!({"kind": kind, "namespace": dep["namespace"], "target": target,
                  "config": dir.join("config.toml").to_string_lossy()}),
        )
    }

    /// Promote a project version one environment up (gated).
    pub async fn promote(&self, pv: &str, env: &str, by: &str) -> Result<Value> {
        self.deploy(pv, env, by, "promote", true).await
    }

    /// Roll an environment back to the project version it ran before (gated).
    pub async fn rollback(&self, env: &str, by: &str) -> Result<Value> {
        let Some(target) = self.store.rollback_target(env).await? else {
            return Err(AgentError::Workflow(format!(
                "{env} has no earlier project version to roll back to"
            )));
        };
        let current: Vec<String> = self
            .store
            .current_deployments(env)
            .await?
            .iter()
            .filter_map(|d| d["project_version"].as_str().map(str::to_string))
            .collect();
        let out = self.deploy(&target, env, by, "rollback", false).await?;
        if out["deployed"] == true {
            self.store.mark_rolled_back_from(env, &target).await?;
            for c in current {
                self.store.mark_rolled_back_from(env, &c).await?;
            }
        }
        let mut out = out;
        out["rolled_back_to"] = json!(target);
        Ok(out)
    }

    /// Bisect: the first project version whose tests fail, along the line of versions from
    /// `good` to `bad` (each `based_on` the one before; hotfix branches elsewhere are ignored).
    pub async fn bisect(&self, good: &str, bad: &str, test: Option<&str>) -> Result<Value> {
        let good = self.store.pv_id(good)?;
        let bad = self.store.pv_id(bad)?;
        // walk back from bad to good
        let mut ordered: Vec<Value> = Vec::new();
        let mut cur = Some(bad.clone());
        while let Some(id) = cur {
            let d = self.store.get_node(&id).await?;
            ordered.push(d.clone());
            if id == good {
                break;
            }
            cur = d["based_on"].as_str().map(str::to_string);
            if ordered.len() > 10_000 {
                break;
            }
        }
        if ordered
            .last()
            .map(|d| d["_id"] != json!(good))
            .unwrap_or(true)
        {
            return Err(AgentError::Workflow(
                "good is not an earlier project version on the line of bad (based_on)".into(),
            ));
        }
        if ordered.len() < 2 {
            return Err(AgentError::Workflow("good must be older than bad".into()));
        }
        ordered.reverse();
        let mut cache: BTreeMap<usize, bool> = BTreeMap::new();
        let (mut lo, mut hi) = (0usize, ordered.len() - 1);
        let mut steps = Vec::new();
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            let id = ordered[mid]["_id"].as_str().unwrap_or_default().to_string();
            let g = self.store.load_code_graph_at(Some(&id)).await?;
            let tests: Vec<String> = g
                .symbols
                .values()
                .filter(|s| {
                    s.kind == Kind::Test
                        && test
                            .map(|t| s.id == t || s.display_name == t)
                            .unwrap_or(true)
                })
                .map(|s| s.id.clone())
                .collect();
            let out = self
                .runner
                .run_tests(self.store, &g, &tests, false, None, None)
                .await?;
            let pass = out.iter().all(|o| o.status == "pass");
            cache.insert(mid, pass);
            steps.push(json!({"project_version": id, "pass": pass}));
            if pass {
                lo = mid
            } else {
                hi = mid
            }
        }
        let first_bad = ordered[hi]["_id"].clone();
        let diff = self
            .store
            .diff_project_versions(
                ordered[lo]["_id"].as_str().unwrap_or_default(),
                first_bad.as_str().unwrap_or_default(),
            )
            .await?;
        Ok(
            json!({"first_bad": first_bad, "last_good": ordered[lo]["_id"], "steps": steps, "diff": diff}),
        )
    }
}
