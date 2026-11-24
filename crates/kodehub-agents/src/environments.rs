//! Environment agents (doc 06): deploy releases and gate promotion. Gates are deterministic
//! checks (decisions.md D-027); results are recorded on the `deployed_to` edge.
//!
//! - → test:    the release builds for every target (Rust lib + Python package).
//! - → qa:      every release test passes; no requirement fails; no orphans; the tests of
//!   live use cases are in the release.
//! - → preprod: smoke run (every release test re-executed).
//! - → prod:    smoke run, and a human approved (a one-time approval issued to the app).
//!
//! Promotion is ordered (one environment up at a time); patch releases and rollbacks run the
//! gate of the environment they enter.

use std::collections::BTreeMap;

use kodehub_core::model::{now_iso, Kind, Language};
use kodehub_core::CodeGraph;
use kodehub_runner::{BuildTarget, Runner};
use kodehub_store::releases::{EnvDef, Selected, SelectionRule};
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
    /// Create a release (default: the stable graph) in the lowest environment.
    pub async fn cut(&self, name: Option<&str>, notes: &str, by: &str) -> Result<Value> {
        self.create(name, notes, &SelectionRule::default(), by)
            .await
    }

    pub async fn create(
        &self,
        name: Option<&str>,
        notes: &str,
        rule: &SelectionRule,
        by: &str,
    ) -> Result<Value> {
        let rel = self
            .store
            .create_release(
                name,
                notes,
                rule,
                by,
                &["rust-lib".into(), "python-pkg".into()],
            )
            .await?;
        let key = rel["_key"].as_str().unwrap_or_default().to_string();
        if let Some(env) = self.store.release_environment(&key).await? {
            self.write_env_config(&env, &key)?;
        }
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

    /// The checks for `release` entering `env` (spec §16.3). `approved`: a human approval was
    /// redeemed for this promotion. `ordered`: the release must be in the environment just below.
    pub async fn gate_checks(
        &self,
        release: &str,
        env: &str,
        approved: bool,
        ordered: bool,
    ) -> Result<Value> {
        let envs = self.store.env_defs().await?;
        let def = envs
            .iter()
            .find(|e| e.name == env)
            .cloned()
            .ok_or_else(|| AgentError::Workflow(format!("unknown environment {env}")))?;
        let mut checks = Vec::new();
        if ordered {
            if def.order == 0 {
                return Err(AgentError::Workflow(format!(
                    "releases enter {env} when they are created or cloned"
                )));
            }
            let prev = &envs[def.order - 1].name;
            let here = self.store.release_environment(release).await?;
            checks.push(json!({"check": format!("currently in {prev}"), "ok": here.as_deref() == Some(prev.as_str()),
                               "in": here}));
        }
        let g = self.store.release_graph(release).await?;
        match Self::gate_kind(&def) {
            "build" => {
                let b = self.build_gate(&g).await?;
                checks.extend(b["checks"].as_array().cloned().unwrap_or_default());
            }
            "tests" => {
                let (_, t) = self.tests_gate(&g, "all release tests pass", false).await?;
                checks.push(t);
                // evaluated on the release's own snapshot, not the working graph
                let rep = self.store.release_trace_report(release).await?;
                let failing = rep["failing_requirements"].clone();
                let orphans = rep["orphans"].clone();
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
                // live use cases' expected-outcome tests must be in the release (spec §16.3)
                let missing = self.live_use_case_tests_missing(&g).await?;
                checks.push(json!({"check": "live use cases are covered by the release", "ok": missing.is_empty(), "missing": missing}));
            }
            "smoke" => {
                let (_, t) = self.tests_gate(&g, "smoke run (re-executed)", true).await?;
                checks.push(t);
            }
            "approval" => {
                let (_, t) = self.tests_gate(&g, "smoke run (re-executed)", true).await?;
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

    /// v0.05 signature: ordered promotion gate.
    pub async fn gate(&self, release: &str, env: &str, approved: bool) -> Result<Value> {
        self.gate_checks(release, env, approved, true).await
    }

    async fn live_use_case_tests_missing(&self, g: &CodeGraph) -> Result<Vec<String>> {
        let rows = self
            .store
            .aql(
                "FOR u IN @@use_cases FILTER u.state == 'live'
                   FOR e IN @@expected_by FILTER e._from == u._id RETURN PARSE_IDENTIFIER(e._to).key",
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

    /// Gate, then move the release up one environment.
    pub async fn promote(
        &self,
        release: &str,
        env: &str,
        approved: bool,
        by: &str,
    ) -> Result<Value> {
        let gate = self.gate_checks(release, env, approved, true).await?;
        if gate["ok"] != json!(true) {
            return Ok(json!({"promoted": false, "gate": gate}));
        }
        let d = self
            .store
            .enter_environment(release, env, by, gate.clone(), "promoted")
            .await?;
        self.write_env_config(env, release)?;
        Ok(json!({"promoted": true, "gate": gate, "deployment": d}))
    }

    /// Patch release (spec §16.2): clone `release` at its current environment with `swaps`,
    /// run that environment's gate on the clone, and on success replace the original there.
    pub async fn patch(
        &self,
        release: &str,
        swaps: &[Selected],
        approved: bool,
        by: &str,
    ) -> Result<Value> {
        let env = self
            .store
            .release_environment(release)
            .await?
            .ok_or_else(|| AgentError::Workflow(format!("{release} is in no environment")))?;
        let clone = self
            .store
            .clone_release(release, &env, swaps, None, by)
            .await?;
        let key = clone["_key"].as_str().unwrap_or_default().to_string();
        let lowest =
            self.store.env_defs().await?.first().map(|e| e.name.clone()) == Some(env.clone());
        if lowest {
            // an open DEV clone: nothing to gate
            return Ok(
                json!({"patched": true, "release": key, "environment": env, "gate": Value::Null}),
            );
        }
        let gate = self.gate_checks(&key, &env, approved, false).await?;
        if gate["ok"] != json!(true) {
            return Ok(json!({"patched": false, "release": key, "environment": env, "gate": gate}));
        }
        let d = self
            .store
            .enter_environment(&key, &env, by, gate.clone(), "patch")
            .await?;
        self.write_env_config(&env, &key)?;
        Ok(
            json!({"patched": true, "release": key, "environment": env, "gate": gate, "deployment": d}),
        )
    }

    /// Rollback (spec §16.3): promote the most recently retired release back into `env`,
    /// through the same gate.
    pub async fn rollback(&self, env: &str, approved: bool, by: &str) -> Result<Value> {
        let Some(target) = self.store.rollback_target(env).await? else {
            return Err(AgentError::Workflow(format!(
                "{env} has no earlier release to roll back to"
            )));
        };
        let current: Vec<String> = self
            .store
            .releases_in(env)
            .await?
            .iter()
            .filter_map(|r| r["release"].as_str().map(str::to_string))
            .collect();
        let gate = self.gate_checks(&target, env, approved, false).await?;
        if gate["ok"] != json!(true) {
            return Ok(json!({"rolled_back": false, "target": target, "gate": gate}));
        }
        let d = self
            .store
            .enter_environment(&target, env, by, gate.clone(), "rollback")
            .await?;
        self.store.mark_rolled_back_from(env, &target).await?;
        for c in current {
            // the release rolled away from is not a rollback target any more
            self.store.mark_rolled_back_from(env, &c).await?;
        }
        self.write_env_config(env, &target)?;
        Ok(
            json!({"rolled_back": true, "rolled_back_to": target, "release": target, "gate": gate, "deployment": d}),
        )
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
