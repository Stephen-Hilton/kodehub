//! The orchestrator's container work (spec §3.1, §5.3–5.6): claim `test_run` work items, make
//! sure a test bundle exists for each language (one build container per bundle key, ever), run
//! the bundle in a test container, record **formal** test runs, and turn new failures into
//! high-priority rework items. Stateless: every step reads and writes ArangoDB and storage, so
//! any number of orchestrator replicas can run.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use kodehub_core::model::{now_iso, Kind, Language};
use kodehub_core::CodeGraph;
use kodehub_runner::jobs::{JobRuntime, JobSpec};
use kodehub_runner::Runner;
use kodehub_storage::{artifact_key, StorageClient, BUCKET};
use kodehub_store::queue::{NewWorkItem, PRIORITY_HIGH};
use kodehub_store::Store;
use serde_json::{json, Value};

use crate::Events;

#[derive(Clone)]
pub struct Orchestrator {
    pub store: Store,
    pub runner: Runner,
    pub storage: StorageClient,
    pub jobs: JobRuntime,
    pub owner: String,
    pub events: Events,
}

/// Container work types this loop runs.
pub const CONTAINER_WORK: &[&str] = &["test_run"];

impl Orchestrator {
    /// Claim and run one container work item. `Ok(false)` when there was nothing to do.
    pub async fn tick(&self) -> anyhow::Result<bool> {
        let types: Vec<String> = CONTAINER_WORK.iter().map(|s| s.to_string()).collect();
        let Some(w) = self.store.claim_next(&self.owner, &types, 1800).await? else {
            return Ok(false);
        };
        let key = w["_key"].as_str().unwrap_or_default().to_string();
        let project = w["project"].as_str().unwrap_or_default().to_string();
        let target = w["target"].as_str().unwrap_or("project").to_string();
        let res = kodehub_store::with_project(project.clone(), async {
            self.test_target(&project, &target, Some(&key)).await
        })
        .await;
        match res {
            Ok(summary) => {
                self.store
                    .complete_work_item(
                        &key,
                        &self.owner,
                        json!({"tested": target}),
                        summary.clone(),
                    )
                    .await?;
                kodehub_store::with_project(project, async {
                    self.events
                        .emit("test.result", json!({"work_item": key, "summary": summary}));
                })
                .await;
            }
            Err(e) => {
                tracing::warn!("test run {key} failed: {e:#}");
                // infrastructure failure: give the item back for another try after a pause
                self.store
                    .fail_work_item(&key, &self.owner, &format!("{e:#}"), json!({}))
                    .await?;
            }
        }
        Ok(true)
    }

    /// Tests that cover `target`: `project` = every test; a C4 node = tests reaching anything
    /// beneath it; a symbol = tests reaching it (or itself, for a test).
    pub async fn tests_for_target(
        &self,
        g: &CodeGraph,
        target: &str,
    ) -> anyhow::Result<Vec<String>> {
        if target == "project" || target == "philosophy/philosophy" {
            return Ok(g
                .symbols
                .values()
                .filter(|s| s.kind == Kind::Test)
                .map(|s| s.id.clone())
                .collect());
        }
        let mut syms: BTreeSet<String> = BTreeSet::new();
        if let Some(k) = target.strip_prefix("symbols/") {
            syms.insert(k.to_string());
        } else {
            let rows = self
                .store
                .aql(
                    "FOR v IN 1..10 OUTBOUND @n @@contains FILTER STARTS_WITH(v._id, CONCAT(@pfx, 'symbols/')) RETURN v._key",
                    json!({"n": target}),
                )
                .await?;
            syms.extend(rows.iter().filter_map(|v| v.as_str().map(str::to_string)));
        }
        let mut tests = BTreeSet::new();
        for s in &syms {
            if g.symbols.get(s).is_some_and(|x| x.kind == Kind::Test) {
                tests.insert(s.clone());
            } else if g.symbols.contains_key(s) {
                tests.extend(g.tests_reaching(s));
            }
        }
        Ok(tests.into_iter().collect())
    }

    async fn job(
        &self,
        kind: &str,
        ctype: Option<&str>,
        name: &str,
        input: &Value,
    ) -> anyhow::Result<Value> {
        let id = format!(
            "{}-{}",
            name,
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        let project = kodehub_store::request_project().unwrap_or_else(|| "_".into());
        let in_key = format!("projects/{project}/jobs/{id}/input.json");
        let out_key = format!("projects/{project}/jobs/{id}/output.json");
        self.storage
            .put(BUCKET, &in_key, serde_json::to_vec(input)?)
            .await?;
        let outcome = self
            .jobs
            .run(&JobSpec {
                kind: kind.into(),
                container_type: ctype.map(str::to_string),
                input_key: in_key,
                output_key: out_key.clone(),
                timeout: Duration::from_secs(if kind == "build" { 1800 } else { 900 }),
                env: vec![],
                name: name.into(),
            })
            .await?;
        let out = match self.storage.get(BUCKET, &out_key).await? {
            Some(b) => serde_json::from_slice(&b)?,
            None => {
                json!({"ok": false, "error": format!("{kind} job wrote no output: {}", outcome.logs)})
            }
        };
        let mut out = out;
        out["job"] = json!({"runtime": outcome.runtime, "name": outcome.job, "ok": outcome.ok,
                            "duration_ms": outcome.duration_ms});
        Ok(out)
    }

    /// Run the formal tests for a target in containers and record them.
    pub async fn test_target(
        &self,
        project: &str,
        target: &str,
        work_item: Option<&str>,
    ) -> anyhow::Result<Value> {
        let g = self.store.load_code_graph().await?;
        let tests = self.tests_for_target(&g, target).await?;
        let mut by_lang: BTreeMap<Language, Vec<String>> = BTreeMap::new();
        for t in &tests {
            if let Some(s) = g.symbols.get(t) {
                by_lang.entry(s.language).or_default().push(t.clone());
            }
        }
        let (mut passed, mut failed, mut built, mut reused) = (0usize, 0usize, 0usize, 0usize);
        let mut regressions = Vec::new();
        let mut bundles = Vec::new();
        for (lang, ids) in by_lang {
            let bkey = self.runner.bundle_key(&g, lang, &ids)?;
            let akey = artifact_key(project, &bkey, &format!("{}-test", lang.as_str()));
            let mut build_log = None;
            if self.storage.exists(BUCKET, &akey).await? {
                reused += 1;
            } else {
                let out = self
                    .job(
                        "build",
                        None,
                        &format!("{}-{}", work_item.unwrap_or("adhoc"), lang.as_str()),
                        &json!({"project": project, "language": lang, "tests": ids, "graph": g, "artifact_key": akey}),
                    )
                    .await?;
                if out["ok"] == true {
                    built += 1;
                    self.record_artifact(&bkey, lang, &akey, &out["manifest"], &g, &ids)
                        .await?;
                } else {
                    build_log = Some(
                        out["log"]
                            .as_str()
                            .or(out["error"].as_str())
                            .unwrap_or("build failed")
                            .to_string(),
                    );
                }
            }
            let results: Vec<Value> = match &build_log {
                Some(log) => ids
                    .iter()
                    .map(|t| {
                        let s = &g.symbols[t];
                        json!({"id": t, "name": s.display_name, "version": s.version,
                               "closure_hash": g.closure_hash(t).unwrap_or_default(),
                               "status": "fail", "output": format!("build failed:\n{log}"), "duration_ms": 0})
                    })
                    .collect(),
                None => {
                    let out = self
                        .job(
                            "test",
                            Some(lang.as_str()),
                            &format!("{}-{}", work_item.unwrap_or("adhoc"), lang.as_str()),
                            &json!({"artifact_key": akey, "tests": ids}),
                        )
                        .await?;
                    if out["ok"] != true {
                        anyhow::bail!("test job failed: {}", out["error"]);
                    }
                    out["results"].as_array().cloned().unwrap_or_default()
                }
            };
            bundles.push(
                json!({"language": lang, "bundle": bkey, "artifact": akey, "tests": ids.len()}),
            );
            for r in &results {
                let pass = r["status"] == "pass";
                if pass {
                    passed += 1;
                } else {
                    failed += 1;
                    if self.is_new_failure(r).await? {
                        regressions.push(r["id"].clone());
                    }
                }
                self.store
                    .db
                    .insert(
                        "test_runs",
                        &json!({
                            "test_symbol": r["id"], "test_name": r["name"], "test_version": r["version"],
                            "closure_hash": r["closure_hash"], "status": r["status"],
                            "output": r["output"], "duration_ms": r["duration_ms"], "ran_at": now_iso(),
                            "formal": true, "run_by": "test-container", "work_item": work_item,
                            "bundle": bkey, "scope": target,
                        }),
                        None,
                    )
                    .await?;
            }
        }
        // failures at any tier become high-priority rework (spec §5.6, §5.7)
        let mut rework = Vec::new();
        for t in &regressions {
            if let Some(k) = self
                .queue_rework(project, &g, t.as_str().unwrap_or_default())
                .await?
            {
                rework.push(k);
            }
        }
        Ok(
            json!({"target": target, "tests": tests.len(), "passed": passed, "failed": failed,
                  "bundles_built": built, "bundles_reused": reused, "bundles": bundles,
                  "regressions": regressions, "rework": rework, "runtime": self.jobs.name()}),
        )
    }

    /// A failure is new when the test's previous formal result (at any closure) was a pass,
    /// or it never ran formally before.
    async fn is_new_failure(&self, r: &Value) -> anyhow::Result<bool> {
        let prev = self
            .store
            .aql(
                "FOR t IN @@test_runs FILTER t.test_symbol == @s AND t.formal != false
                   SORT t.ran_at DESC LIMIT 1 RETURN t.status",
                json!({"s": r["id"]}),
            )
            .await?;
        Ok(prev.first().and_then(Value::as_str) != Some("fail"))
    }

    async fn queue_rework(
        &self,
        project: &str,
        g: &CodeGraph,
        test: &str,
    ) -> anyhow::Result<Option<Value>> {
        // one open rework item per failing test
        let open = self
            .store
            .list_work_items(Some(project), None, 500)
            .await?
            .into_iter()
            .any(|w| {
                matches!(w["state"].as_str(), Some("queued" | "claimed"))
                    && w["failing_tests"]
                        .as_array()
                        .is_some_and(|f| f.iter().any(|x| x == test))
            });
        if open {
            return Ok(None);
        }
        // the code the test exercises (its closure), nearest first
        let target = g
            .edges
            .iter()
            .find(|e| e.from == test && e.kind.is_build_dependency())
            .map(|e| format!("symbols/{}", e.to))
            .or_else(|| {
                g.edges
                    .iter()
                    .find(|e| e.to == test && e.kind == kodehub_core::model::CodeEdgeKind::TestedBy)
                    .map(|e| format!("symbols/{}", e.from))
            })
            .unwrap_or_else(|| format!("symbols/{test}"));
        let w = self
            .store
            .enqueue(&NewWorkItem {
                project: project.into(),
                agent_type: "coding".into(),
                kind: "fix_failure".into(),
                target: Some(target),
                priority: PRIORITY_HIGH,
                task: json!({"failing_test": test, "note": "a formal test run found a new failure; fix the code or the test"}),
                failing_tests: vec![test.to_string()],
                requested_by: Some("orchestrator".into()),
                ..Default::default()
            })
            .await?;
        Ok(Some(w["_key"].clone()))
    }

    /// Artifact node, linked to the versions it was built from (spec §5.2).
    async fn record_artifact(
        &self,
        bkey: &str,
        lang: Language,
        akey: &str,
        manifest: &Value,
        g: &CodeGraph,
        ids: &[String],
    ) -> anyhow::Result<()> {
        let target = format!("{}-test", lang.as_str());
        let k = blake3::hash(format!("{bkey}|{target}").as_bytes()).to_hex()[..24].to_string();
        self.store
            .db
            .upsert(
                "artifacts",
                &json!({"_key": k, "closure_hash": bkey, "build_target": target, "status": "ok",
                        "storage_key": akey, "built_at": now_iso(), "tests": manifest["tests"].as_array().map(|a| a.len()),
                        "toolchain": manifest["toolchain"], "path": akey}),
                None,
            )
            .await?;
        for t in ids {
            if let Some(s) = g.symbols.get(t) {
                let from = format!("artifacts/{k}");
                let to = format!("versions/{}", s.version);
                self.store
                    .db
                    .insert_ignore(
                        "built_from",
                        &json!({"_key": kodehub_store::edge_key("built_from", &from, &to), "_from": from, "_to": to}),
                        None,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Run forever: claim container work, sleep when idle.
    pub fn spawn(self) {
        tokio::spawn(async move {
            loop {
                match self.tick().await {
                    Ok(true) => continue,
                    Ok(false) => tokio::time::sleep(Duration::from_secs(2)).await,
                    Err(e) => {
                        tracing::warn!("orchestrator tick: {e:#}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
        });
    }
}
