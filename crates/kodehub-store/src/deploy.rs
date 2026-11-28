//! Environments, deploy targets, deployers, deployments (D-059, D-060; docs/ontology.md
//! "Deploying").
//!
//! - A **project version** is the release (`tree.rs`): open while work is accepted into it,
//!   frozen on its first deployment beyond the lowest environment (or when someone freezes it).
//! - **Environments** come from the project's `environments` setting (lowest first).
//! - A **deploy target** is a logical place code runs (`default`, `aws`); cloud-specific
//!   versions say `deployed_to` it, everything else uses `default`.
//! - A **deployer** is exactly one (environment, deploy target) pair: the real setup (kind
//!   `k8s_namespace` today: the environment's namespace in this cluster), with `runs` to the
//!   image that performs the deploy.
//! - A **deployment** is one project version going into one environment. It `uses` the
//!   environment's deployers for the targets the version needs, is retryable (pending →
//!   awaiting_approval → running → succeeded | failed), and `logs_to` immutable deploy logs.
//!   A succeeded deployment supersedes earlier ones in the same environment that served the
//!   same targets, so an environment can hold several project versions at once (one per
//!   cloud, or a hotfix next to the next release).

use std::collections::{BTreeMap, BTreeSet};

use kodehub_core::model::now_iso;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::store::{edge_key, key_of};
use crate::{Result, Store, StoreError};

/// The deploy target every project has; versions without `deployed_to` use it.
pub const DEFAULT_TARGET: &str = "default";
/// The image that performs `k8s_namespace` deployments.
pub const DEPLOY_IMAGE: &str = "deploy_k8s";

/// One environment of a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvDef {
    pub name: String,
    pub order: usize,
    pub max_releases: Option<u64>,
    pub requires_human_approval: bool,
}

const DEPLOY_WRITE: &[&str] = &[
    "deployments",
    "deploy_logs",
    "has_deployment",
    "uses",
    "logs_to",
    "project_versions",
];

impl Store {
    // ------------------------------------------------------------------ setup

    /// The project's environments, lowest first, synced from the `environments` setting,
    /// with a deployer for every deploy target in each.
    pub async fn env_defs(&self) -> Result<Vec<EnvDef>> {
        let setting = match self.project_id() {
            Some(p) => self.setting(&p, "environments").await?,
            None => Value::Null,
        };
        let list = setting.as_array().cloned().unwrap_or_else(|| {
            crate::settings::definition("environments")
                .map(|d| (d.default)())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
        });
        let defs: Vec<EnvDef> = list
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                Some(EnvDef {
                    name: e["name"].as_str()?.to_string(),
                    order: i,
                    max_releases: e["max_releases"].as_u64(),
                    requires_human_approval: e["requires_human_approval"] == true,
                })
            })
            .collect();
        for d in &defs {
            let key = self.db.pkey(&d.name)?;
            let existing = self.db.get("environments", &key, None).await;
            let doc = json!({"_key": key, "name": d.name, "order": d.order,
                             "max_releases": d.max_releases,
                             "requires_human_approval": d.requires_human_approval,
                             "config": {"stubs_allowed": d.order == 0}});
            match existing {
                Ok(e)
                    if e["order"] == doc["order"]
                        && e["max_releases"] == doc["max_releases"]
                        && e["requires_human_approval"] == doc["requires_human_approval"] => {}
                _ => {
                    self.db.upsert("environments", &doc, None).await?;
                }
            }
        }
        self.ensure_deploy_setup(&defs).await?;
        Ok(defs)
    }

    async fn env_def(&self, name: &str) -> Result<EnvDef> {
        self.env_defs()
            .await?
            .into_iter()
            .find(|d| d.name == name)
            .ok_or_else(|| StoreError::NotFound(format!("environment {name}")))
    }

    /// The default deploy target, the deploy image, and one deployer per (environment, target).
    pub async fn ensure_deploy_setup(&self, defs: &[EnvDef]) -> Result<()> {
        let project = self.db.require_project()?;
        let image_key = self.db.pkey(DEPLOY_IMAGE)?;
        if self
            .db
            .get("container_images", &image_key, None)
            .await
            .is_err()
        {
            self.db
                .insert_ignore(
                    "container_images",
                    &json!({"_key": image_key, "name": DEPLOY_IMAGE, "purpose": "deploy",
                            "image": "kodehub/worker:dev", "layers": ["core"],
                            "description": "Deploys a project version's containers into a Kubernetes namespace",
                            "source": "default", "created_at": now_iso()}),
                    None,
                )
                .await?;
            self.own(&format!("container_images/{image_key}")).await?;
        }
        let tkey = self.db.pkey(DEFAULT_TARGET)?;
        if self.db.get("deploy_targets", &tkey, None).await.is_err() {
            self.db
                .insert_ignore(
                    "deploy_targets",
                    &json!({"_key": tkey, "name": DEFAULT_TARGET,
                            "description": "Where code runs unless a version names another target",
                            "created_at": now_iso()}),
                    None,
                )
                .await?;
            self.own(&format!("deploy_targets/{tkey}")).await?;
        }
        let targets = self
            .aql("FOR t IN @@deploy_targets RETURN t", json!({}))
            .await?;
        for d in defs {
            let env_id = self.db.pid("environments", &d.name)?;
            self.own(&env_id).await?;
            for t in &targets {
                let tname = t["name"].as_str().unwrap_or(DEFAULT_TARGET);
                let dkey = self.db.pkey(&format!("{}-{}", d.name, tname))?;
                if self.db.get("deployers", &dkey, None).await.is_ok() {
                    continue;
                }
                let did = format!("deployers/{dkey}");
                let tid = t["_id"].as_str().unwrap_or_default().to_string();
                self.db
                    .insert_ignore(
                        "deployers",
                        &json!({"_key": dkey, "name": format!("{tname} in {}", d.name),
                                "environment": d.name, "target": tname, "kind": "k8s_namespace",
                                "namespace": format!("kodehub-{project}-{}", d.name).replace('_', "-"),
                                "created_at": now_iso()}),
                        None,
                    )
                    .await?;
                for (from, coll) in [(&env_id, "has_deployer"), (&tid, "has_deployer")] {
                    self.db
                        .insert_ignore(
                            coll,
                            &json!({"_key": edge_key(coll, from, &did), "_from": from, "_to": did}),
                            None,
                        )
                        .await?;
                }
                let img = format!("container_images/{image_key}");
                self.db
                    .insert_ignore(
                        "runs",
                        &json!({"_key": edge_key("runs", &did, &img), "_from": did, "_to": img}),
                        None,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Environments with what runs in each now.
    pub async fn environments(&self) -> Result<Vec<Value>> {
        let defs = self.env_defs().await?;
        let mut out = Vec::new();
        for d in defs {
            let env = self
                .db
                .get("environments", &self.db.pkey(&d.name)?, None)
                .await?;
            let current = self.current_deployments(&d.name).await?;
            let deployers = self
                .aql(
                    "FOR e IN @@has_deployer FILTER e._from == @env RETURN DOCUMENT(e._to)",
                    json!({"env": env["_id"]}),
                )
                .await?;
            let mut v = env.clone();
            v["running"] = json!(current);
            v["deployers"] = json!(deployers);
            out.push(v);
        }
        Ok(out)
    }

    // ------------------------------------------------------------------ deployments

    /// Deploy targets a project version needs: `default`, plus every target one of its
    /// versions names with `deployed_to`.
    pub async fn targets_of(&self, pv: &str) -> Result<Vec<String>> {
        let tree = self.tree(Some(pv)).await?;
        let versions: Vec<&String> = tree.version_of.values().collect();
        let rows = self
            .aql(
                "FOR e IN @@deployed_to FILTER e._from IN @v RETURN DISTINCT DOCUMENT(e._to).name",
                json!({"v": versions}),
            )
            .await?;
        let mut out: BTreeSet<String> = rows
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        out.insert(DEFAULT_TARGET.to_string());
        Ok(out.into_iter().collect())
    }

    /// Create a deployment of a project version into an environment. `ordered`: the project
    /// version must currently run in the environment just below (normal promotion; hotfixes
    /// and rollbacks are not ordered).
    pub async fn create_deployment(
        &self,
        pv: &str,
        env: &str,
        by: &str,
        reason: &str,
        ordered: bool,
    ) -> Result<Value> {
        let pv = self.pv_id(pv)?;
        let pv_doc = self.get_node(&pv).await?;
        let envs = self.env_defs().await?;
        let def = envs
            .iter()
            .find(|e| e.name == env)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(format!("environment {env}")))?;
        if ordered && def.order > 0 {
            let below = &envs[def.order - 1].name;
            let running_below = self
                .current_deployments(below)
                .await?
                .iter()
                .any(|d| d["project_version"] == json!(pv));
            if !running_below {
                return Err(StoreError::Invalid(format!(
                    "{} reaches {env} from {below}; deploy it to {below} first",
                    pv_doc["name"].as_str().unwrap_or(&pv)
                )));
            }
        }
        let env_id = self.db.pid("environments", env)?;
        let targets = self.targets_of(&pv).await?;
        let mut deployers = Vec::new();
        for t in &targets {
            let dkey = self.db.pkey(&format!("{env}-{t}"))?;
            match self.db.get("deployers", &dkey, None).await {
                Ok(d) => deployers.push(d["_id"].as_str().unwrap_or_default().to_string()),
                Err(_) => {
                    return Err(StoreError::Invalid(format!(
                        "{env} has no deployer for target `{t}`"
                    )))
                }
            }
        }
        let state = if def.requires_human_approval {
            "awaiting_approval"
        } else {
            "pending"
        };
        let n = self
            .aql("RETURN COUNT(FOR d IN @@deployments RETURN 1)", json!({}))
            .await?
            .first()
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        let key = self.db.pkey(&format!("d{n}"))?;
        let id = format!("deployments/{key}");
        let trx = self.db.begin(DEPLOY_WRITE, &[]).await?;
        let res: Result<Value> = async {
            let d = self
                .db
                .insert(
                    "deployments",
                    &json!({"_key": key, "name": format!("d{n}"), "project_version": pv,
                            "project_version_name": pv_doc["name"], "environment": env,
                            "targets": targets, "state": state, "reason": reason,
                            "ordered": ordered, "attempts": 0, "requested_by": by,
                            "created_at": now_iso(), "updated_at": now_iso()}),
                    Some(&trx),
                )
                .await?;
            for from in [&pv, &env_id] {
                self.db
                    .insert(
                        "has_deployment",
                        &json!({"_key": edge_key("has_deployment", from, &id), "_from": from, "_to": id}),
                        Some(&trx),
                    )
                    .await?;
            }
            for dep in &deployers {
                // a deployment only uses deployers of its own environment (D-060)
                let dd = self.db.get("deployers", key_of(dep), Some(&trx)).await?;
                if dd["environment"] != json!(env) {
                    return Err(StoreError::Invariant(format!(
                        "{dep} belongs to {}, not {env}",
                        dd["environment"]
                    )));
                }
                self.db
                    .insert(
                        "uses",
                        &json!({"_key": edge_key("uses", &id, dep), "_from": id, "_to": dep}),
                        Some(&trx),
                    )
                    .await?;
            }
            Ok(d)
        }
        .await;
        match res {
            Ok(d) => {
                self.db.commit(&trx).await?;
                self.deploy_log(&id, "requested", "ok", json!({"by": by, "reason": reason}))
                    .await?;
                Ok(d)
            }
            Err(e) => {
                let _ = self.db.abort(&trx).await;
                Err(e)
            }
        }
    }

    pub async fn get_deployment(&self, key: &str) -> Result<Value> {
        let key = key_of(key);
        let d = self.db.get("deployments", key, None).await?;
        let logs = self
            .aql(
                "FOR e IN @@logs_to FILTER e._from == @d LET l = DOCUMENT(e._to)
                   SORT l.at RETURN UNSET(l, '_rev')",
                json!({"d": d["_id"]}),
            )
            .await?;
        let uses = self
            .aql(
                "FOR e IN @@uses FILTER e._from == @d RETURN DOCUMENT(e._to)",
                json!({"d": d["_id"]}),
            )
            .await?;
        let mut v = d;
        v["logs"] = json!(logs);
        v["deployers"] = json!(uses);
        Ok(v)
    }

    pub async fn list_deployments(&self, env: Option<&str>) -> Result<Vec<Value>> {
        self.aql(
            "FOR d IN @@deployments FILTER @e == null OR d.environment == @e
               SORT d.created_at DESC LIMIT 200 RETURN d",
            json!({"e": env}),
        )
        .await
    }

    /// Append an immutable deploy log entry to a deployment.
    pub async fn deploy_log(
        &self,
        deployment: &str,
        step: &str,
        status: &str,
        detail: Value,
    ) -> Result<Value> {
        let did = self.db.qid(&if deployment.contains('/') {
            deployment.to_string()
        } else {
            format!("deployments/{deployment}")
        })?;
        let d = self.get_node(&did).await?;
        let l = self
            .db
            .insert(
                "deploy_logs",
                &json!({"deployment": did, "attempt": d["attempts"], "step": step,
                        "status": status, "detail": detail, "at": now_iso()}),
                None,
            )
            .await?;
        let lid = l["_id"].as_str().unwrap_or_default().to_string();
        self.db
            .insert(
                "logs_to",
                &json!({"_key": edge_key("logs_to", &did, &lid), "_from": did, "_to": lid}),
                None,
            )
            .await?;
        Ok(l)
    }

    /// A human approves a deployment waiting for approval (production).
    pub async fn approve_deployment(&self, key: &str, by: &str) -> Result<Value> {
        let key = key_of(key);
        let d = self.db.get("deployments", key, None).await?;
        if d["state"] != "awaiting_approval" {
            return Err(StoreError::Conflict(format!(
                "deployment {key} is {}, not awaiting approval",
                d["state"]
            )));
        }
        self.db
            .patch(
                "deployments",
                key,
                &json!({"state": "pending", "approved_by": by, "approved_at": now_iso(),
                        "updated_at": now_iso()}),
                None,
            )
            .await?;
        self.deploy_log(key, "approved", "ok", json!({"by": by}))
            .await?;
        self.get_deployment(key).await
    }

    /// Start an attempt: pending | failed → running.
    pub async fn start_deployment(&self, key: &str) -> Result<Value> {
        let key = key_of(key);
        let d = self.db.get("deployments", key, None).await?;
        if !matches!(d["state"].as_str(), Some("pending") | Some("failed")) {
            return Err(StoreError::Conflict(format!(
                "deployment {key} is {}",
                d["state"]
            )));
        }
        if d["state"] == "failed" && d["approved_by"].is_null() {
            let def = self
                .env_def(d["environment"].as_str().unwrap_or(""))
                .await?;
            if def.requires_human_approval {
                return Err(StoreError::Conflict(format!(
                    "deployment {key} needs a human approval"
                )));
            }
        }
        let attempts = d["attempts"].as_i64().unwrap_or(0) + 1;
        let v = self
            .db
            .patch(
                "deployments",
                key,
                &json!({"state": "running", "attempts": attempts, "started_at": now_iso(),
                        "updated_at": now_iso()}),
                None,
            )
            .await?;
        self.deploy_log(key, "attempt", "running", json!({"attempt": attempts}))
            .await?;
        Ok(v)
    }

    /// Finish an attempt. On success the deployment supersedes earlier ones in its environment
    /// that served the same targets, the environment's `max_releases` limit is applied, and the
    /// project version is frozen when the environment is above the lowest.
    pub async fn finish_deployment(
        &self,
        key: &str,
        ok: bool,
        gate: Value,
        by: &str,
    ) -> Result<Value> {
        let _write = self.write_lock.lock().await;
        let key = key_of(key).to_string();
        let d = self.db.get("deployments", &key, None).await?;
        if d["state"] != "running" {
            return Err(StoreError::Conflict(format!(
                "deployment {key} is {}, not running",
                d["state"]
            )));
        }
        let now = now_iso();
        if !ok {
            self.db
                .patch(
                    "deployments",
                    &key,
                    &json!({"state": "failed", "gate": gate, "finished_at": now, "updated_at": now}),
                    None,
                )
                .await?;
            self.deploy_log(&key, "finished", "failed", gate).await?;
            return self.get_deployment(&key).await;
        }
        let env = d["environment"].as_str().unwrap_or_default().to_string();
        let def = self.env_def(&env).await?;
        let targets: BTreeSet<String> = d["targets"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let mut superseded = Vec::new();
        let current = self.current_deployments(&env).await?;
        for c in &current {
            let ct: BTreeSet<String> = c["targets"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            if !ct.is_disjoint(&targets) {
                superseded.push(c.clone());
            }
        }
        let mut remaining: Vec<Value> = current
            .iter()
            .filter(|c| !superseded.iter().any(|s| s["_key"] == c["_key"]))
            .cloned()
            .collect();
        if let Some(max) = def.max_releases {
            remaining.sort_by(|a, b| a["finished_at"].as_str().cmp(&b["finished_at"].as_str()));
            while remaining.len() as u64 + 1 > max && !remaining.is_empty() {
                superseded.push(remaining.remove(0));
            }
        }
        for s in &superseded {
            self.db
                .patch(
                    "deployments",
                    s["_key"].as_str().unwrap_or_default(),
                    &json!({"superseded_by": d["_id"], "superseded_at": now, "updated_at": now}),
                    None,
                )
                .await?;
        }
        self.db
            .patch(
                "deployments",
                &key,
                &json!({"state": "succeeded", "gate": gate, "finished_at": now, "updated_at": now,
                        "finished_by": by}),
                None,
            )
            .await?;
        if def.order > 0 {
            // a project version is cut — immutable — on its first deployment beyond the lowest
            // environment (hard rule 4)
            self.freeze_project_version(
                d["project_version"].as_str().unwrap_or_default(),
                by,
                &format!("deployed to {env}"),
            )
            .await?;
        }
        self.deploy_log(
            &key,
            "finished",
            "succeeded",
            json!({"superseded": superseded.iter().map(|s| s["_id"].clone()).collect::<Vec<_>>()}),
        )
        .await?;
        self.get_deployment(&key).await
    }

    /// Succeeded deployments in an environment that nothing has superseded (what runs there).
    pub async fn current_deployments(&self, env: &str) -> Result<Vec<Value>> {
        self.aql(
            "FOR d IN @@deployments FILTER d.environment == @e AND d.state == 'succeeded'
               AND d.superseded_by == null
               SORT d.finished_at DESC RETURN d",
            json!({"e": env}),
        )
        .await
    }

    /// The project version to roll back to in `env`: the latest superseded succeeded
    /// deployment there that has not been rolled away from.
    pub async fn rollback_target(&self, env: &str) -> Result<Option<String>> {
        let current: Vec<Value> = self
            .current_deployments(env)
            .await?
            .iter()
            .map(|d| d["project_version"].clone())
            .collect();
        Ok(self
            .aql(
                "FOR d IN @@deployments FILTER d.environment == @e AND d.state == 'succeeded'
                   AND d.superseded_by != null AND d.rolled_back != true AND d.project_version NOT IN @cur
                   SORT d.superseded_at DESC LIMIT 1 RETURN d.project_version",
                json!({"e": env, "cur": current}),
            )
            .await?
            .first()
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// Mark the deployments of a project version in an environment as rolled away from, so a
    /// second rollback does not pick it again.
    pub async fn mark_rolled_back_from(&self, env: &str, pv: &str) -> Result<()> {
        let pv = self.pv_id(pv)?;
        self.aql(
            "FOR d IN @@deployments FILTER d.environment == @e AND d.project_version == @pv
               AND d.superseded_by != null
               UPDATE d WITH {rolled_back: true} IN @@deployments",
            json!({"e": env, "pv": pv}),
        )
        .await
        .map(|_| ())
    }

    /// Where a project version runs now.
    pub async fn running_in(&self, pv: &str) -> Result<Vec<String>> {
        let pv = self.pv_id(pv)?;
        Ok(self
            .aql(
                "FOR d IN @@deployments FILTER d.project_version == @pv AND d.state == 'succeeded'
                   AND d.superseded_by == null RETURN DISTINCT d.environment",
                json!({"pv": pv}),
            )
            .await?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect())
    }

    // ------------------------------------------------------------------ builds

    /// Record a build (an artifact keyed by `closure_hash` and target), with `build_of` edges to
    /// the versions it was made from and `built_for` the deploy target (default `default`).
    /// Idempotent: the same closure and target is the same build.
    pub async fn record_build(
        &self,
        closure_hash: &str,
        target: &str,
        doc: Value,
        versions: &[String],
        deploy_target: Option<&str>,
    ) -> Result<Value> {
        let key = self
            .db
            .pkey(&blake3::hash(format!("{closure_hash}|{target}").as_bytes()).to_hex()[..24])?;
        let mut d = doc;
        d["_key"] = json!(key);
        d["closure_hash"] = json!(closure_hash);
        d["target"] = json!(target);
        if d.get("built_at").is_none() {
            d["built_at"] = json!(now_iso());
        }
        // builds are immutable: an existing build of the same closure is the same artifact
        let id = format!("builds/{key}");
        if self.get_node(&id).await.is_err() {
            self.db.insert_ignore("builds", &d, None).await?;
        }
        for v in versions {
            let vid = if v.contains('/') {
                self.db.qid(v)?
            } else {
                // a bare version key: code or test version
                let k = self.db.pkey(v)?;
                if self.db.get("test_versions", &k, None).await.is_ok() {
                    format!("test_versions/{k}")
                } else {
                    format!("code_versions/{k}")
                }
            };
            if self.get_node(&vid).await.is_err() {
                continue;
            }
            self.db
                .insert_ignore(
                    "build_of",
                    &json!({"_key": edge_key("build_of", &id, &vid), "_from": id, "_to": vid}),
                    None,
                )
                .await?;
        }
        let tid = self
            .db
            .pid("deploy_targets", deploy_target.unwrap_or(DEFAULT_TARGET))?;
        if self.get_node(&tid).await.is_ok() {
            self.db
                .insert_ignore(
                    "built_for",
                    &json!({"_key": edge_key("built_for", &id, &tid), "_from": id, "_to": tid}),
                    None,
                )
                .await?;
        }
        self.get_node(&id).await
    }

    // ------------------------------------------------------------------ project versions

    /// A project version with what it holds and where it runs.
    pub async fn get_project_version(&self, pv: &str) -> Result<Value> {
        let id = self.pv_id(pv)?;
        let mut d = self.get_node(&id).await?;
        let tree = self.tree(Some(&id)).await?;
        d["nodes"] = json!(tree.version_of.len());
        d["code"] = json!(tree.nodes_in("code").count());
        d["tests"] = json!(tree.nodes_in("tests").count());
        d["running_in"] = json!(self.running_in(&id).await?);
        d["current"] = json!(self.current_version().await? == id);
        d["deployments"] = json!(
            self.aql(
                "FOR e IN @@has_deployment FILTER e._from == @pv LET x = DOCUMENT(e._to)
                   SORT x.created_at RETURN {key: x._key, environment: x.environment, state: x.state,
                                             superseded: x.superseded_by != null, at: x.finished_at || x.created_at}",
                json!({"pv": id}),
            )
            .await?
        );
        Ok(d)
    }

    /// Every project version with where it runs, newest first.
    pub async fn project_versions_overview(&self) -> Result<Vec<Value>> {
        let cur = self.current_version().await?;
        let mut out = Vec::new();
        for v in self.list_project_versions().await? {
            let id = v["_id"].as_str().unwrap_or_default().to_string();
            let mut x = v.clone();
            x["running_in"] = json!(self.running_in(&id).await?);
            x["current"] = json!(id == cur);
            out.push(x);
        }
        Ok(out)
    }

    /// A hotfix: a new open project version based on a frozen one, starting with its tree.
    pub async fn create_hotfix(&self, frozen: &str, name: Option<&str>, by: &str) -> Result<Value> {
        let id = self.pv_id(frozen)?;
        let d = self.get_node(&id).await?;
        if d["state"] != "frozen" {
            return Err(StoreError::Invalid(format!(
                "{} is open; change it directly instead of hotfixing it",
                d["name"].as_str().unwrap_or(&id)
            )));
        }
        let name = name
            .map(str::to_string)
            .unwrap_or_else(|| format!("{}-hotfix", d["name"].as_str().unwrap_or("r")));
        let mut v = self
            .create_project_version(Some(&name), Some(&id), by)
            .await?;
        self.db
            .patch(
                "project_versions",
                v["_key"].as_str().unwrap_or_default(),
                &json!({"hotfix_of": id}),
                None,
            )
            .await?;
        v["hotfix_of"] = json!(id);
        Ok(v)
    }

    /// What changed between two project versions, node by node.
    pub async fn diff_project_versions(&self, a: &str, b: &str) -> Result<Value> {
        let (ta, tb) = (self.tree(Some(a)).await?, self.tree(Some(b)).await?);
        let all: BTreeSet<String> = ta
            .version_of
            .keys()
            .chain(tb.version_of.keys())
            .cloned()
            .collect();
        let ids: Vec<&String> = all.iter().collect();
        let info: BTreeMap<String, Value> = self
            .aql(
                "FOR id IN @ids LET d = DOCUMENT(id)
                   RETURN {id, name: d.display_name || d.title || d.name, kind: d.kind}",
                json!({"ids": ids}),
            )
            .await?
            .into_iter()
            .filter_map(|r| Some((r["id"].as_str()?.to_string(), r)))
            .collect();
        let (mut added, mut removed, mut changed) = (Vec::new(), Vec::new(), Vec::new());
        for n in &all {
            let row = |v: Option<&str>| {
                let mut r = info.get(n).cloned().unwrap_or(json!({"id": n}));
                r["version"] = json!(v);
                r["level"] = json!(crate::c4::level_of(n));
                r
            };
            match (ta.version(n), tb.version(n)) {
                (None, Some(v)) => added.push(row(Some(v))),
                (Some(v), None) => removed.push(row(Some(v))),
                (Some(x), Some(y)) if x != y => {
                    let mut r = row(Some(y));
                    r["from"] = json!(x);
                    r["to"] = json!(y);
                    r["symbol"] = json!(key_of(n));
                    changed.push(r);
                }
                _ => {}
            }
        }
        Ok(
            json!({"from": ta.root, "to": tb.root, "added": added, "removed": removed,
                  "changed": changed}),
        )
    }

    /// Traceability of one project version (gates): requirements and their status, failing
    /// and unverified requirements, orphans.
    pub async fn project_version_trace(&self, pv: &str) -> Result<Value> {
        let v = self.view(Some(pv)).await?;
        let mut reqs = Vec::new();
        let (mut failing, mut unverified, mut at_risk) = (Vec::new(), Vec::new(), Vec::new());
        for (id, r) in &v.requirements {
            if r["kind"] == "decision" {
                continue;
            }
            let tests = v.test_rows(&v.requirement_tests(id));
            let status = if tests.is_empty() {
                "uncovered"
            } else if tests.iter().any(|t| t.status == "fail") {
                "fail"
            } else if tests.iter().any(|t| t.status != "pass") {
                "at_risk"
            } else {
                "pass"
            };
            match status {
                "fail" => failing.push(r["_key"].clone()),
                "uncovered" => unverified.push(r["_key"].clone()),
                _ => {}
            }
            if r["at_risk"] == true || status == "at_risk" {
                at_risk.push(r["_key"].clone());
            }
            reqs.push(
                json!({"id": id, "key": r["_key"], "title": r["title"], "status": status,
                             "tests": tests}),
            );
        }
        let orphans: Vec<Value> = crate::queries::orphans_of(&v)
            .into_iter()
            .map(|o| o["id"].clone())
            .collect();
        Ok(json!({"project_version": v.pv, "requirements": reqs,
                  "failing_requirements": failing, "unverified_requirements": unverified,
                  "at_risk_requirements": at_risk, "orphans": orphans}))
    }
}
