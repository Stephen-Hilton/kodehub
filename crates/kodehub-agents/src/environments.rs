//! Environment agents (doc 06): deploy releases and gate promotion. Gates are deterministic
//! checks (decisions.md D-027); results are recorded on the `deployed_to` edge.
//!
//! - → test: the release builds for every target (Rust lib + Python package).
//! - → qa:   every test in the release passes; no requirement fails; no orphans.
//! - → prod: the QA smoke run (all release tests executed in the qa environment) passes, and a
//!   human approved (`approved_by: human`).
//!
//! Promotion is ordered: a release must already have been deployed to the previous environment.

use std::collections::BTreeMap;

use kodehub_core::model::{now_iso, Kind, Language, ENVIRONMENTS};
use kodehub_core::CodeGraph;
use kodehub_runner::{BuildTarget, Runner};
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
    /// Cut a release and record it as running in `dev`.
    pub async fn cut(&self, name: Option<&str>, notes: &str, by: &str) -> Result<Value> {
        let rel = self
            .store
            .cut_release(name, notes, by, &["rust-lib".into(), "python-pkg".into()])
            .await?;
        let key = rel["_key"].as_str().unwrap_or_default().to_string();
        self.store
            .deploy(&key, "dev", by, json!({"cut": true}))
            .await?;
        self.write_env_config("dev", &key)?;
        Ok(rel)
    }

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
                        roots: r,
                        namespaces,
                    },
                )
                .await?;
            if !b.cache_hit {
                self.runner.record_artifact(self.store, &b).await?;
            }
            ok &= b.ok;
            checks.push(json!({"check": format!("build {}", b.target), "ok": b.ok, "cache_hit": b.cache_hit,
                               "detail": if b.ok { String::new() } else { b.log.chars().take(2000).collect() }}));
        }
        Ok(json!({"ok": ok, "checks": checks}))
    }

    /// `force` re-executes every test (the QA smoke run) instead of trusting cached passes.
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

    /// Check the gate for promoting `release` into `env`.
    /// `approved`: a human approval was redeemed for this promotion (prod only).
    pub async fn gate(&self, release: &str, env: &str, approved: bool) -> Result<Value> {
        let idx = ENVIRONMENTS
            .iter()
            .position(|e| *e == env)
            .ok_or_else(|| AgentError::Workflow(format!("unknown environment {env}")))?;
        if idx == 0 {
            return Err(AgentError::Workflow(
                "releases enter dev when they are cut".into(),
            ));
        }
        let prev = ENVIRONMENTS[idx - 1];
        let deployed_prev = self
            .store
            .aql("FOR e IN @@deployed_to FILTER e._from == CONCAT(@pfx, 'releases/', @r) AND e._to == CONCAT(@pfx, 'environments/', @p) LIMIT 1 RETURN 1",
                 json!({"r": release, "p": prev}))
            .await?;
        let mut checks = vec![
            json!({"check": format!("deployed to {prev} first"), "ok": !deployed_prev.is_empty()}),
        ];
        let g = self.store.release_graph(release).await?;
        match env {
            "test" => {
                let b = self.build_gate(&g).await?;
                checks.extend(b["checks"].as_array().cloned().unwrap_or_default());
            }
            "qa" => {
                let (_, t) = self.tests_gate(&g, "all release tests pass", false).await?;
                checks.push(t);
                // evaluated on the release's own snapshot, not the working graph (review M6 #6)
                let rep = self.store.release_trace_report(release).await?;
                let failing = rep["failing_requirements"].clone();
                let orphans = rep["orphans"].clone();
                // requirements at risk (edited, not yet covered) or unverified at cut block qa (final #10)
                let snapshot_risk: Vec<Value> = self.store.get_release(release).await?["trace"]
                    ["requirements"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|r| r["at_risk"] == json!(true))
                    .map(|r| r["key"].clone())
                    .collect();
                let mut risky = rep["unverified_requirements"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                risky.extend(snapshot_risk);
                checks.push(json!({"check": "no requirement at risk or unverified", "ok": risky.is_empty(), "requirements": risky}));
                checks.push(json!({"check": "no failing requirements", "ok": failing.as_array().map(|a| a.is_empty()).unwrap_or(false), "failing": failing}));
                checks.push(json!({"check": "no orphans", "ok": orphans.as_array().map(|a| a.is_empty()).unwrap_or(false), "orphans": orphans}));
            }
            "prod" => {
                // a real smoke run: every release test re-executed (review M6 #7)
                let (_, t) = self
                    .tests_gate(&g, "QA smoke run (re-executed)", true)
                    .await?;
                checks.push(t);
                checks.push(json!({"check": "human approval", "ok": approved, "approved_by": if approved { "human" } else { "" }}));
            }
            _ => {}
        }
        let ok = checks.iter().all(|c| c["ok"] == json!(true));
        Ok(
            json!({"ok": ok, "environment": env, "release": release, "checks": checks, "checked_at": now_iso()}),
        )
    }

    /// Gate, then deploy. Returns the gate report (and deployment when it passed).
    pub async fn promote(
        &self,
        release: &str,
        env: &str,
        approved: bool,
        by: &str,
    ) -> Result<Value> {
        let gate = self.gate(release, env, approved).await?;
        if gate["ok"] != json!(true) {
            return Ok(json!({"promoted": false, "gate": gate}));
        }
        let d = self.store.deploy(release, env, by, gate.clone()).await?;
        self.write_env_config(env, release)?;
        Ok(json!({"promoted": true, "gate": gate, "deployment": d}))
    }

    /// Simulated environments: a directory + config file per environment (doc 06).
    fn write_env_config(&self, env: &str, release: &str) -> Result<()> {
        let dir = self.runner.home.join("envs").join(env);
        std::fs::create_dir_all(&dir).map_err(|e| AgentError::Workflow(e.to_string()))?;
        let cfg = format!("# simulated {env} environment\nenvironment = \"{env}\"\nrelease = \"{release}\"\ndeployed_at = \"{}\"\nstubs_allowed = {}\n", now_iso(), env == "dev");
        std::fs::write(dir.join("config.toml"), cfg)
            .map_err(|e| AgentError::Workflow(e.to_string()))
    }

    /// Bisect: find the first release (between good and bad, by cut time) whose tests fail.
    pub async fn bisect(&self, good: &str, bad: &str, test: Option<&str>) -> Result<Value> {
        let all = self.store.list_releases().await?; // newest first
        let mut ordered: Vec<Value> = all.into_iter().rev().collect();
        let gi = ordered
            .iter()
            .position(|r| r["_key"] == json!(good))
            .ok_or_else(|| AgentError::Workflow("good release not found".into()))?;
        let bi = ordered
            .iter()
            .position(|r| r["_key"] == json!(bad))
            .ok_or_else(|| AgentError::Workflow("bad release not found".into()))?;
        if gi >= bi {
            return Err(AgentError::Workflow("good must be older than bad".into()));
        }
        ordered = ordered[gi..=bi].to_vec();
        let mut cache: BTreeMap<usize, bool> = BTreeMap::new();
        let (mut lo, mut hi) = (0usize, ordered.len() - 1);
        let mut steps = Vec::new();
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            let key = ordered[mid]["_key"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let g = self.store.release_graph(&key).await?;
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
            steps.push(json!({"release": key, "pass": pass}));
            if pass {
                lo = mid
            } else {
                hi = mid
            }
        }
        let first_bad = ordered[hi]["_key"].clone();
        let diff = self
            .store
            .diff_releases(
                ordered[lo]["_key"].as_str().unwrap_or_default(),
                first_bad.as_str().unwrap_or_default(),
            )
            .await?;
        Ok(
            json!({"first_bad": first_bad, "last_good": ordered[lo]["_key"], "steps": steps, "diff": diff}),
        )
    }
}
