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
pub const CONTAINER_WORK: &[&str] = &["test_run", "git_import", "git_export", "backup"];

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
        let kind = w["agent_type"].as_str().unwrap_or("test_run").to_string();
        let res = kodehub_store::with_project(project.clone(), async {
            match kind.as_str() {
                "git_import" => self.git_import(&project, &w).await,
                "git_export" => self.git_export(&project, &w).await,
                "backup" => self.backup(&project).await,
                _ => self.test_target(&project, &target, Some(&key)).await,
            }
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

    /// A Git credential for the project (layered: acting user, project, owner), passed to the
    /// container only as a secret environment variable.
    async fn git_secret(&self, project: &str) -> anyhow::Result<Vec<(String, String)>> {
        Ok(self
            .store
            .resolve_git_credential(project, None)
            .await?
            .map(|s| vec![("GIT_TOKEN".to_string(), s.expose().to_string())])
            .unwrap_or_default())
    }

    async fn job_with(
        &self,
        kind: &str,
        name: &str,
        input: &Value,
        secret_env: Vec<(String, String)>,
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
                container_type: None,
                input_key: in_key,
                output_key: out_key.clone(),
                timeout: Duration::from_secs(1800),
                env: vec![],
                secret_env,
                name: name.into(),
            })
            .await?;
        match self.storage.get(BUCKET, &out_key).await? {
            Some(b) => Ok(serde_json::from_slice(&b)?),
            None => Ok(
                json!({"ok": false, "error": format!("{kind} job wrote no output: {}", outcome.logs)}),
            ),
        }
    }

    /// Git import (spec §13.1): the deterministic container clones and parses; the units are
    /// staged and an import agent is asked for the C4 mapping.
    async fn git_import(&self, project: &str, w: &Value) -> anyhow::Result<Value> {
        let key = w["_key"].as_str().unwrap_or_default();
        let out = self
            .job_with(
                "git-import",
                key,
                &w["task"],
                self.git_secret(project).await?,
            )
            .await?;
        if out["ok"] != true {
            anyhow::bail!(
                "git import failed: {}",
                out["error"].as_str().unwrap_or("?")
            );
        }
        let units = out["units"].as_array().cloned().unwrap_or_default();
        let n = self.store.stage_import(key, &out, &units).await?;
        let brief: Vec<Value> = units
            .iter()
            .map(|u| json!({"id": u["id"], "kind": u["kind"], "language": u["language"], "file": u["file"],
                            "package": u["package"], "module": u["module"], "decomposed": u["decomposed"]}))
            .collect();
        let agent = self
            .store
            .enqueue(&NewWorkItem {
                project: project.into(),
                agent_type: "import".into(),
                kind: "map_import".into(),
                target: Some("philosophy/philosophy".into()),
                priority: kodehub_store::queue::PRIORITY_NORMAL,
                task: json!({"import": key, "repo_url": out["repo_url"], "commit": out["commit"],
                             "summary": out["summary"], "units": brief,
                             "note": "answer with one import_mapping action naming this import"}),
                requested_by: w["requested_by"].as_str().map(str::to_string),
                ..Default::default()
            })
            .await?;
        Ok(
            json!({"staged": n, "commit": out["commit"], "summary": out["summary"], "import_agent": agent["_key"]}),
        )
    }

    /// Git export (spec §13.2): render the live versions as ordinary files and push them.
    async fn git_export(&self, project: &str, w: &Value) -> anyhow::Result<Value> {
        let files = self.export_files(project).await?;
        let mut input = w["task"].clone();
        input["files"] = json!(files);
        let out = self
            .job_with(
                "git-export",
                w["_key"].as_str().unwrap_or("export"),
                &input,
                self.git_secret(project).await?,
            )
            .await?;
        if out["ok"] != true {
            anyhow::bail!(
                "git export failed: {}",
                out["error"].as_str().unwrap_or("?")
            );
        }
        Ok(out)
    }

    /// Back up the project's graph into storage (hybrid storage ships it off-site).
    pub async fn backup(&self, project: &str) -> anyhow::Result<Value> {
        let dump = self.store.export_project(project).await?;
        let key = format!(
            "projects/{project}/backups/db-{}.json",
            now_iso().replace([':', '.'], "-")
        );
        let bytes = serde_json::to_vec(&dump)?;
        self.storage.put(BUCKET, &key, bytes.clone()).await?;
        Ok(json!({"key": key, "bytes": bytes.len()}))
    }

    /// The project as files: one rendered crate / package per language (tests included) and a
    /// `KODEHUB.md` describing the C4 graph. Kodehub stays the source of truth.
    pub async fn export_files(&self, project: &str) -> anyhow::Result<BTreeMap<String, String>> {
        let g = self.store.load_code_graph().await?;
        let mut files = BTreeMap::new();
        for lang in [Language::Rust, Language::Python] {
            let roots: Vec<String> = g
                .symbols
                .values()
                .filter(|s| s.language == lang)
                .map(|s| s.id.clone())
                .collect();
            if roots.is_empty() {
                continue;
            }
            let r = kodehub_render::render(&kodehub_render::RenderRequest {
                graph: &g,
                roots,
                language: lang,
                include_tests: true,
                namespaces: vec![],
                harness: None,
            })?;
            for (p, c) in r.files {
                files.insert(format!("{}/{p}", lang.as_str()), c);
            }
        }
        let tree = self
            .store
            .aql(
                "FOR v, e, p IN 1..6 OUTBOUND CONCAT(@pfx, 'philosophy/philosophy') @@contains
                   RETURN {depth: LENGTH(p.edges), id: v._id, name: v.title || v.name || v.display_name}",
                json!({}),
            )
            .await?;
        let mut md = format!(
            "# {project}\n\nExported by Kodehub — this repository is a mirror. Kodehub is the source of truth;\nchanges made here are not merged back.\n\n## Philosophy\n\n{}\n\n## Architecture\n\n",
            self.store
                .get_philosophy()
                .await?
                .and_then(|p| p["text"].as_str().map(str::to_string))
                .unwrap_or_default()
        );
        for n in tree {
            let d = n["depth"].as_u64().unwrap_or(1) as usize;
            md.push_str(&format!(
                "{}- {} (`{}`)\n",
                "  ".repeat(d.saturating_sub(1)),
                n["name"].as_str().unwrap_or(""),
                n["id"].as_str().unwrap_or("")
            ));
        }
        files.insert("KODEHUB.md".into(), md);
        Ok(files)
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
                secret_env: vec![],
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

/// The agent side of the orchestrator (spec §4): claim agent work items (caps enforced by the
/// claim), register the intent, run one ephemeral agent task per step, apply its results, and
/// close, retry, escalate or block the item. Coding work goes through the changeset engine.
#[derive(Clone)]
pub struct AgentDispatcher {
    pub store: Store,
    pub engine: std::sync::Arc<kodehub_agents::Engine>,
    pub owner: String,
    pub events: Events,
}

impl AgentDispatcher {
    pub async fn tick(&self) -> anyhow::Result<bool> {
        let types: Vec<String> = kodehub_store::queue::AGENT_TYPES
            .iter()
            .map(|s| s.to_string())
            .collect();
        let Some(w) = self.store.claim_next(&self.owner, &types, 1800).await? else {
            return Ok(false);
        };
        let key = w["_key"].as_str().unwrap_or_default().to_string();
        let project = w["project"].as_str().unwrap_or_default().to_string();
        let this = self.clone();
        let wk = key.clone();
        kodehub_store::with_project(
            project.clone(),
            kodehub_agents::CURRENT_WORK_ITEM.scope(wk, async move { this.handle(&w).await }),
        )
        .await?;
        Ok(true)
    }

    async fn handle(&self, w: &Value) -> anyhow::Result<()> {
        let key = w["_key"].as_str().unwrap_or_default().to_string();
        let project = w["project"].as_str().unwrap_or_default().to_string();
        let agent_type = w["agent_type"].as_str().unwrap_or("coding").to_string();
        let kind = w["kind"].as_str().unwrap_or("").to_string();
        let target = w["target"]
            .as_str()
            .unwrap_or("philosophy/philosophy")
            .to_string();
        self.events.emit(
            "agent.activity",
            json!({"role": agent_type, "work_item": key, "message": format!("claimed {kind} on {target}")}),
        );
        // declared intent (spec §10.1): the node it will change
        let d = self
            .store
            .register_intent(&key, std::slice::from_ref(&target), &kind, false)
            .await?;
        match d["decision"].as_str() {
            Some("absorbed") | Some("sequenced") => {
                self.events.emit(
                    "agent.activity",
                    json!({"role": agent_type, "work_item": key, "decision": d}),
                );
                return Ok(());
            }
            _ => {}
        }
        let outcome = if agent_type == "coding" {
            self.coding(w, &kind, &target).await
        } else if agent_type == "review" {
            // reviews run inside the changeset engine; a standalone review item is informational
            Ok(Outcome::Done(
                json!({"note": "reviews run inside changesets"}),
            ))
        } else {
            self.role(w, &agent_type).await
        };
        match outcome {
            Ok(Outcome::Done(result)) => {
                let log = result.get("change_log").cloned().unwrap_or(Value::Null);
                self.store
                    .complete_work_item(&key, &self.owner, log, result)
                    .await?;
            }
            Ok(Outcome::Waiting(cs)) => {
                self.store.park_work_item(&key, &self.owner, &cs).await?;
            }
            Ok(Outcome::Failed(reason, log)) => {
                self.store
                    .fail_work_item(&key, &self.owner, &reason, log)
                    .await?;
            }
            Err(kodehub_agents::AgentError::Blocked(msg)) => {
                self.store
                    .notify(
                        &project,
                        "external_blocker",
                        &msg,
                        json!({"work_item": key, "agent_type": agent_type}),
                    )
                    .await?;
                self.store.block_work_item(&key, &self.owner, &msg).await?;
            }
            Err(e) => {
                self.store
                    .fail_work_item(&key, &self.owner, &e.to_string(), json!({}))
                    .await?;
            }
        }
        self.events.emit("work.updated", json!({"work_item": key}));
        Ok(())
    }

    /// Coding work: a changeset driven through plan → tests → code → blind review → formal
    /// tests → promotion. Requirement edits reuse the engine's requirement path.
    async fn coding(&self, w: &Value, kind: &str, target: &str) -> kodehub_agents::Result<Outcome> {
        let cs_key = if kind == "requirement_changed" {
            let req = kodehub_store::key_of(target).to_string();
            let by = w["requested_by"].as_str().unwrap_or("human").to_string();
            self.engine.on_requirement_edited(&req, &by).await?
        } else {
            let reqs: Vec<String> = self
                .store
                .requirements_for(target)
                .await
                .ok()
                .and_then(|c| c["requirements"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter(|r| r["inherited"] == false || r["distance"].as_u64().unwrap_or(9) <= 1)
                .filter_map(|r| r["key"].as_str().map(str::to_string))
                .collect();
            let failing = w["failing_tests"].clone();
            let cs = self
                .store
                .open_changeset(
                    &format!("{} {}", kind.replace('_', " "), target),
                    &format!(
                        "Work item {}: {kind} on {target}. Task: {}. Failing tests: {failing}. Earlier attempts: {}",
                        w["_key"].as_str().unwrap_or(""),
                        w["task"],
                        w["history"]
                    ),
                    &reqs,
                    "orchestrator",
                )
                .await?;
            let k = cs["_key"].as_str().unwrap_or_default().to_string();
            self.store
                .patch_changeset(&k, json!({"work_item": w["_key"], "origin": kind}))
                .await?;
            self.engine.plan(&k).await?;
            k
        };
        self.store
            .patch_changeset(&cs_key, json!({"work_item": w["_key"]}))
            .await?;
        let cs = self.store.get_changeset(&cs_key).await?;
        Ok(match cs["state"].as_str() {
            Some("merged") => Outcome::Done(json!({"changeset": cs_key, "merged": true,
                                                     "change_log": cs["activity"]})),
            Some("planned") => Outcome::Waiting(cs_key),
            Some(other) => Outcome::Failed(
                format!(
                    "changeset {cs_key} ended {other}: {}",
                    cs["error"].as_str().unwrap_or("")
                ),
                json!({"changeset": cs_key, "state": other}),
            ),
            None => Outcome::Failed("changeset vanished".into(), json!({})),
        })
    }

    /// Any other role: one agent call, then its actions applied through the store.
    async fn role(&self, w: &Value, agent_type: &str) -> kodehub_agents::Result<Outcome> {
        let out = self.engine.run_role(agent_type, w).await?;
        let actions = out.json["actions"].as_array().cloned().unwrap_or_default();
        let project = w["project"].as_str().unwrap_or_default().to_string();
        let ctx = kodehub_agents::actions::ApplyCtx {
            store: &self.store,
            project: &project,
            work_item: w,
            agent_type,
        };
        let report = kodehub_agents::actions::apply(&ctx, &actions).await?;
        if let Some(msg) = report["blocked"].as_str() {
            return Err(kodehub_agents::AgentError::Blocked(msg.to_string()));
        }
        Ok(Outcome::Done(
            json!({"change_log": out.json["change_log"], "report": report,
                                 "backend": out.backend, "model": out.model}),
        ))
    }

    pub fn spawn(self) {
        tokio::spawn(async move {
            loop {
                match self.tick().await {
                    Ok(true) => continue,
                    Ok(false) => tokio::time::sleep(Duration::from_millis(500)).await,
                    Err(e) => {
                        tracing::warn!("agent dispatcher: {e:#}");
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                }
            }
        });
    }
}

enum Outcome {
    Done(Value),
    /// Waiting for a human (a plan under `human_engagement = plans`).
    Waiting(String),
    Failed(String, Value),
}

/// The kickoff chain for a new project (spec §14.2): the architect cleans up the philosophy
/// (when it came from a transcript) and evaluates the requirements; then one planning agent per
/// context. Later levels are enqueued by the planning agents themselves.
pub async fn enqueue_kickoff(
    store: &Store,
    project: &str,
    contexts: &[String],
    raw_transcript: bool,
    by: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut prev: Vec<String> = Vec::new();
    let mk = |agent: &str, kind: &str, target: &str, deps: Vec<String>| NewWorkItem {
        project: project.into(),
        agent_type: agent.into(),
        kind: kind.into(),
        target: Some(target.into()),
        priority: kodehub_store::queue::PRIORITY_NORMAL,
        task: json!({"kickoff": true}),
        depends_on: deps,
        requested_by: Some(by.into()),
        ..Default::default()
    };
    if raw_transcript {
        let w = store
            .enqueue(&mk(
                "architect",
                "cleanup_philosophy",
                "philosophy/philosophy",
                vec![],
            ))
            .await?;
        prev = vec![w["_key"].as_str().unwrap_or_default().to_string()];
        out.push(w);
    }
    let ev = store
        .enqueue(&mk(
            "architect",
            "evaluate_requirements",
            "philosophy/philosophy",
            prev,
        ))
        .await?;
    let ev_key = ev["_key"].as_str().unwrap_or_default().to_string();
    out.push(ev);
    for c in contexts {
        out.push(
            store
                .enqueue(&mk("planning", "plan_context", c, vec![ev_key.clone()]))
                .await?,
        );
    }
    Ok(out)
}
