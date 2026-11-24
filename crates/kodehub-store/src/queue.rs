//! Work queue and the claim-and-lease protocol (spec §3.3, §4.4, §4.5).
//!
//! One protocol for every competing consumer (scheduler replicas, orchestrator replicas):
//! a record is claimed by an atomic, revision-checked update in ArangoDB that sets `owner` and
//! `lease_expires`. If another replica got there first the update conflicts and the claimant
//! skips the record. A dead worker's lease expires and the record becomes claimable again.
//!
//! Work items are global records (one queue for every project) carrying their `project`.

use std::collections::BTreeMap;

use kodehub_core::model::now_iso;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Result, Store, StoreError};

pub const WORK_ITEMS: &str = "kodehub_work_items";

/// Queue priorities (higher first). Failures and regressions enter high (spec §4.4).
pub const PRIORITY_HIGH: i64 = 100;
pub const PRIORITY_NORMAL: i64 = 50;
pub const PRIORITY_LOW: i64 = 10;

/// Agent types (spec §4.1, §11.4, §15.4) plus non-agent work done by containers.
pub const AGENT_TYPES: &[&str] = &[
    "coding",
    "test_author",
    "review",
    "planning",
    "architect",
    "import",
    "design",
    "sysadmin",
];
/// Work done by deterministic containers, not agents (no cap, no LLM).
pub const CONTAINER_TYPES: &[&str] = &["build", "test_run", "git_import", "git_export"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NewWorkItem {
    pub project: String,
    /// What runs it: an agent type, or a container type.
    pub agent_type: String,
    /// Free label of the work (`implement`, `fix_failure`, `write_tests`, `plan_context`, …).
    #[serde(default)]
    pub kind: String,
    /// The node the work is about (`components/x`, `symbols/y`, `philosophy/philosophy`).
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "normal")]
    pub priority: i64,
    /// The original task (instructions, context references).
    #[serde(default)]
    pub task: Value,
    #[serde(default)]
    pub failing_tests: Vec<String>,
    /// Work items that must be done before this one may be claimed (kickoff chain).
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub requested_by: Option<String>,
    /// Change-log history carried from earlier attempts (retries, spec §4.5).
    #[serde(default)]
    pub history: Vec<Value>,
    #[serde(default)]
    pub retry_count: i64,
    #[serde(default)]
    pub retry_of: Option<String>,
}

fn normal() -> i64 {
    PRIORITY_NORMAL
}

fn lease_until(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A claim that lost the race, or a record that is no longer claimable.
pub fn is_conflict(e: &StoreError) -> bool {
    matches!(e, StoreError::Arango { num: 1200, .. })
}

impl Store {
    pub async fn enqueue(&self, w: &NewWorkItem) -> Result<Value> {
        if !AGENT_TYPES.contains(&w.agent_type.as_str())
            && !CONTAINER_TYPES.contains(&w.agent_type.as_str())
            && !w.agent_type.starts_with("custom:")
        {
            return Err(StoreError::Invalid(format!(
                "unknown agent type `{}`",
                w.agent_type
            )));
        }
        let key = crate::new_key("wi");
        let now = now_iso();
        let mut doc = serde_json::to_value(w).map_err(|e| StoreError::Invalid(e.to_string()))?;
        doc["_key"] = json!(key);
        doc["state"] = json!("queued");
        doc["owner"] = Value::Null;
        doc["lease_expires"] = Value::Null;
        doc["created_at"] = json!(now);
        doc["updated_at"] = json!(now);
        doc["combined_with"] = json!([]);
        self.g_insert(WORK_ITEMS, &doc).await
    }

    pub async fn work_item(&self, key: &str) -> Result<Value> {
        self.g_get(WORK_ITEMS, key)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("work item {key}")))
    }

    pub async fn list_work_items(
        &self,
        project: Option<&str>,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        self.g_aql(
            "FOR w IN @@w FILTER (@p == null OR w.project == @p) AND (@s == null OR w.state == @s)
               SORT w.state == 'claimed' DESC, w.priority DESC, w.created_at ASC LIMIT @l
               RETURN UNSET(w, '_rev')",
            json!({"@w": WORK_ITEMS, "p": project, "s": state, "l": limit.clamp(1, 2000)}),
        )
        .await
    }

    /// Active claims (unexpired) per (project, agent type).
    pub async fn active_claims(&self) -> Result<BTreeMap<(String, String), u64>> {
        let rows = self
            .g_aql(
                "FOR w IN @@w FILTER w.state == 'claimed' AND w.lease_expires > @now
                   COLLECT p = w.project, t = w.agent_type WITH COUNT INTO n RETURN {p, t, n}",
                json!({"@w": WORK_ITEMS, "now": now_iso()}),
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some((
                    (r["p"].as_str()?.to_string(), r["t"].as_str()?.to_string()),
                    r["n"].as_u64()?,
                ))
            })
            .collect())
    }

    /// Claimable items: queued (or with an expired lease), every dependency done, highest
    /// priority first.
    async fn claimable(&self, agent_types: &[String], limit: usize) -> Result<Vec<Value>> {
        self.g_aql(
            "FOR w IN @@w
               FILTER (w.state == 'queued' OR (w.state == 'claimed' AND w.lease_expires < @now))
               FILTER LENGTH(@types) == 0 OR w.agent_type IN @types
               FILTER w.not_before == null OR w.not_before <= @now
               LET open_deps = LENGTH(FOR d IN (w.depends_on || [])
                                        LET x = DOCUMENT(@@w, d)
                                        FILTER x != null AND x.state NOT IN ['done', 'absorbed', 'cancelled']
                                        RETURN 1)
               FILTER open_deps == 0
               SORT w.priority DESC, w.created_at ASC LIMIT @l
               RETURN w",
            json!({"@w": WORK_ITEMS, "now": now_iso(), "types": agent_types, "l": limit}),
        )
        .await
    }

    /// The claim step of the protocol: an atomic, revision-checked update. `Ok(None)` when
    /// another replica won the record (or it is no longer claimable).
    pub async fn try_claim(
        &self,
        w: &Value,
        owner: &str,
        lease_secs: i64,
    ) -> Result<Option<Value>> {
        let res = self
            .g_aql(
                "LET d = DOCUMENT(@@w, @k)
                 FILTER d != null AND d._rev == @rev
                   AND (d.state == 'queued' OR (d.state == 'claimed' AND d.lease_expires < @now))
                 UPDATE d WITH {state: 'claimed', owner: @o, lease_expires: @until, claimed_at: @now,
                                attempts: (d.attempts || 0) + 1, updated_at: @now}
                   IN @@w OPTIONS {ignoreRevs: false}
                 RETURN NEW",
                json!({"@w": WORK_ITEMS, "k": w["_key"], "rev": w["_rev"], "o": owner,
                       "until": lease_until(lease_secs), "now": now_iso()}),
            )
            .await;
        match res {
            Ok(rows) => Ok(rows.into_iter().next()),
            Err(e) if is_conflict(&e) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Claim the next item an owner may run, honouring per-project, per-type concurrency caps
    /// and the project budget (a project at budget claims nothing new, spec §12.2).
    pub async fn claim_next(
        &self,
        owner: &str,
        agent_types: &[String],
        lease_secs: i64,
    ) -> Result<Option<Value>> {
        let mut claims = self.active_claims().await?;
        let mut cap_cache: BTreeMap<String, Value> = BTreeMap::new();
        let mut stopped: BTreeMap<String, bool> = BTreeMap::new();
        for w in self.claimable(agent_types, 50).await? {
            let project = w["project"].as_str().unwrap_or_default().to_string();
            let t = w["agent_type"].as_str().unwrap_or_default().to_string();
            if !stopped.contains_key(&project) {
                let at_budget = self.project_at_budget(&project).await.unwrap_or(false);
                stopped.insert(project.clone(), at_budget);
            }
            if stopped[&project] {
                continue;
            }
            if AGENT_TYPES.contains(&t.as_str()) || t.starts_with("custom:") {
                if !cap_cache.contains_key(&project) {
                    let caps = self
                        .setting(&project, "agent_caps")
                        .await
                        .unwrap_or(Value::Null);
                    let hard = self
                        .setting(&project, "agent_caps_hard_limit")
                        .await
                        .unwrap_or(Value::Null);
                    cap_cache.insert(project.clone(), json!({"caps": caps, "hard": hard}));
                }
                let c = &cap_cache[&project];
                let cap = c["caps"][&t].as_u64().unwrap_or(1);
                let used = claims
                    .get(&(project.clone(), t.clone()))
                    .copied()
                    .unwrap_or(0);
                // an expired lease of the same type is being re-claimed: it does not add a slot
                let reclaim = w["state"] == "claimed";
                if !reclaim && used >= cap {
                    continue;
                }
                if let Some(hard) = c["hard"].as_u64() {
                    let all: u64 = claims
                        .iter()
                        .filter(|((p, ty), _)| *p == project && AGENT_TYPES.contains(&ty.as_str()))
                        .map(|(_, n)| *n)
                        .sum();
                    if !reclaim && all >= hard {
                        continue;
                    }
                }
            }
            if let Some(claimed) = self.try_claim(&w, owner, lease_secs).await? {
                *claims.entry((project, t)).or_default() += 1;
                return Ok(Some(claimed));
            }
        }
        Ok(None)
    }

    /// Extend a lease the owner still holds.
    pub async fn heartbeat(&self, key: &str, owner: &str, lease_secs: i64) -> Result<bool> {
        let rows = self
            .g_aql(
                "LET d = DOCUMENT(@@w, @k) FILTER d.owner == @o AND d.state == 'claimed'
                 UPDATE d WITH {lease_expires: @until, updated_at: @now} IN @@w RETURN 1",
                json!({"@w": WORK_ITEMS, "k": key, "o": owner, "until": lease_until(lease_secs), "now": now_iso()}),
            )
            .await?;
        Ok(!rows.is_empty())
    }

    async fn owned(&self, key: &str, owner: &str) -> Result<Value> {
        let w = self.work_item(key).await?;
        if w["owner"] != json!(owner) || w["state"] != "claimed" {
            return Err(StoreError::Conflict(format!(
                "work item {key} is not claimed by {owner} (lease lost?)"
            )));
        }
        Ok(w)
    }

    /// Finish an item: record the change log and result, release the claim.
    pub async fn complete_work_item(
        &self,
        key: &str,
        owner: &str,
        change_log: Value,
        result: Value,
    ) -> Result<Value> {
        let w = self.owned(key, owner).await?;
        let mut hist = w["history"].as_array().cloned().unwrap_or_default();
        hist.push(
            json!({"at": now_iso(), "attempt": w["attempts"], "outcome": "done",
                         "change_log": change_log, "by": owner}),
        );
        let done = self
            .g_patch(
                WORK_ITEMS,
                key,
                &json!({"state": "done", "owner": null, "lease_expires": null, "result": result,
                        "history": hist, "done_at": now_iso(), "updated_at": now_iso()}),
            )
            .await?;
        self.revoke_work_item_tokens(key).await?;
        // absorbed items complete with their survivor (spec §10.2)
        let project = w["project"].as_str().unwrap_or_default().to_string();
        for a in w["combined_with"].as_array().cloned().unwrap_or_default() {
            if let Some(a) = a.as_str() {
                self.g_patch(
                    WORK_ITEMS,
                    a,
                    &json!({"state": "done", "done_at": now_iso(), "done_with": key}),
                )
                .await?;
                self.close_intents(&project, a, "done").await?;
            }
        }
        self.close_intents(&project, key, "done").await?;
        Ok(done)
    }

    /// An attempt failed to fix its problem: retry as a fresh work item carrying the full
    /// history, or escalate to an architect agent after `retry_limit` rework cycles (spec §4.5).
    /// Returns the follow-up item.
    pub async fn fail_work_item(
        &self,
        key: &str,
        owner: &str,
        reason: &str,
        change_log: Value,
    ) -> Result<Value> {
        let w = self.owned(key, owner).await?;
        let project = w["project"].as_str().unwrap_or_default().to_string();
        let mut hist = w["history"].as_array().cloned().unwrap_or_default();
        hist.push(
            json!({"at": now_iso(), "attempt": w["attempts"], "outcome": "failed",
                         "reason": reason, "change_log": change_log, "by": owner}),
        );
        self.g_patch(
            WORK_ITEMS,
            key,
            &json!({"state": "failed", "owner": null, "lease_expires": null, "history": hist,
                    "failure": reason, "updated_at": now_iso()}),
        )
        .await?;
        self.revoke_work_item_tokens(key).await?;
        self.close_intents(w["project"].as_str().unwrap_or_default(), key, "failed")
            .await?;
        let limit = self
            .setting(&project, "retry_limit")
            .await?
            .as_i64()
            .unwrap_or(3);
        let retries = w["retry_count"].as_i64().unwrap_or(0) + 1;
        let escalate = retries >= limit && w["agent_type"] != "architect";
        let next = NewWorkItem {
            project,
            agent_type: if escalate {
                "architect".into()
            } else {
                w["agent_type"].as_str().unwrap_or("coding").into()
            },
            kind: if escalate {
                "resolve_hard_failure".into()
            } else {
                w["kind"].as_str().unwrap_or_default().into()
            },
            target: w["target"].as_str().map(str::to_string),
            priority: PRIORITY_HIGH,
            task: if escalate {
                json!({"escalated_from": key, "original": w["task"], "reason": reason,
                       "note": "the retry limit was reached; decide using the merged requirements and the philosophy document"})
            } else {
                w["task"].clone()
            },
            failing_tests: w["failing_tests"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            depends_on: vec![],
            requested_by: w["requested_by"].as_str().map(str::to_string),
            history: hist,
            retry_count: if escalate { 0 } else { retries },
            retry_of: Some(key.to_string()),
        };
        let item = self.enqueue(&next).await?;
        // a combined task stays combined: absorbed items follow the retry (counted once)
        if let Some(c) = w["combined_with"].as_array().filter(|c| !c.is_empty()) {
            let nk = item["_key"].as_str().unwrap_or_default().to_string();
            self.g_patch(WORK_ITEMS, &nk, &json!({"combined_with": c}))
                .await?;
            for a in c.iter().filter_map(Value::as_str) {
                self.g_patch(WORK_ITEMS, a, &json!({"absorbed_into": nk}))
                    .await?;
            }
        }
        if escalate {
            self.g_patch(WORK_ITEMS, key, &json!({"escalated_to": item["_key"]}))
                .await?;
        }
        Ok(item)
    }

    /// Give a claim back without counting an attempt (e.g. the item must wait on another).
    pub async fn release_work_item(
        &self,
        key: &str,
        owner: &str,
        depends_on: &[String],
    ) -> Result<Value> {
        let w = self.owned(key, owner).await?;
        let mut deps: Vec<String> = w["depends_on"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        deps.extend(depends_on.iter().cloned());
        deps.sort();
        deps.dedup();
        self.g_patch(
            WORK_ITEMS,
            key,
            &json!({"state": "queued", "owner": null, "lease_expires": null, "depends_on": deps,
                    "attempts": (w["attempts"].as_i64().unwrap_or(1) - 1).max(0), "updated_at": now_iso()}),
        )
        .await
    }

    /// An external blocker (no credential, revoked key, missing permission): park the item
    /// without counting an attempt; a human must act first (spec §4.5).
    pub async fn block_work_item(&self, key: &str, owner: &str, reason: &str) -> Result<Value> {
        self.owned(key, owner).await?;
        self.revoke_work_item_tokens(key).await?;
        self.g_patch(
            WORK_ITEMS,
            key,
            &json!({"state": "blocked", "owner": null, "lease_expires": null, "blocked_reason": reason,
                    "updated_at": now_iso()}),
        )
        .await
    }

    /// Put blocked items of a project back in the queue (after a human fixed the blocker).
    pub async fn unblock_work_items(&self, project: &str) -> Result<u64> {
        let rows = self
            .g_aql(
                "FOR w IN @@w FILTER w.project == @p AND w.state == 'blocked'
                   UPDATE w WITH {state: 'queued', blocked_reason: null, updated_at: @now} IN @@w RETURN 1",
                json!({"@w": WORK_ITEMS, "p": project, "now": now_iso()}),
            )
            .await?;
        Ok(rows.len() as u64)
    }

    /// The work waits for a human (a plan to approve): release the claim, keep the link.
    pub async fn park_work_item(&self, key: &str, owner: &str, changeset: &str) -> Result<Value> {
        self.owned(key, owner).await?;
        self.g_patch(
            WORK_ITEMS,
            key,
            &json!({"state": "waiting_human", "owner": null, "lease_expires": null,
                    "changeset": changeset, "updated_at": now_iso()}),
        )
        .await
    }

    /// Finish the work item linked to a changeset once the changeset ends (merged / rejected).
    pub async fn settle_changeset_item(
        &self,
        changeset: &str,
        merged: bool,
        note: &str,
    ) -> Result<Option<Value>> {
        let rows = self
            .g_aql(
                "FOR w IN @@w FILTER w.changeset == @c AND w.state IN ['waiting_human', 'claimed'] RETURN w",
                json!({"@w": WORK_ITEMS, "c": changeset}),
            )
            .await?;
        let Some(w) = rows.into_iter().next() else {
            return Ok(None);
        };
        let key = w["_key"].as_str().unwrap_or_default().to_string();
        let owner = format!("settle-{changeset}");
        // take the item over, then finish it like any other
        self.g_patch(
            WORK_ITEMS,
            &key,
            &json!({"state": "claimed", "owner": owner, "lease_expires": lease_until(300)}),
        )
        .await?;
        let out = if merged {
            self.complete_work_item(
                &key,
                &owner,
                json!({"changeset": changeset, "note": note}),
                json!({"merged": true}),
            )
            .await?
        } else {
            self.fail_work_item(&key, &owner, note, json!({"changeset": changeset}))
                .await?
        };
        Ok(Some(out))
    }

    pub async fn cancel_work_item(&self, key: &str, by: &str) -> Result<Value> {
        let w = self.work_item(key).await?;
        if matches!(w["state"].as_str(), Some("done" | "cancelled" | "absorbed")) {
            return Ok(w);
        }
        self.g_patch(
            WORK_ITEMS,
            key,
            &json!({"state": "cancelled", "owner": null, "lease_expires": null,
                    "cancelled_by": by, "updated_at": now_iso()}),
        )
        .await
    }

    /// Queue summary for a project (UI work-queue view).
    pub async fn queue_stats(&self, project: &str) -> Result<Value> {
        let rows = self
            .g_aql(
                "FOR w IN @@w FILTER w.project == @p COLLECT s = w.state, t = w.agent_type WITH COUNT INTO n
                   RETURN {state: s, agent_type: t, n}",
                json!({"@w": WORK_ITEMS, "p": project}),
            )
            .await?;
        Ok(json!({"project": project, "counts": rows,
                  "at_budget": self.project_at_budget(project).await.unwrap_or(false)}))
    }

    /// Work items touching each node (in-node "in progress" badge, spec §8.3).
    pub async fn in_progress_by_node(&self, project: &str) -> Result<BTreeMap<String, u64>> {
        let rows = self
            .g_aql(
                "FOR w IN @@w FILTER w.project == @p AND w.state IN ['queued', 'claimed'] AND w.target != null
                   COLLECT t = w.target WITH COUNT INTO n RETURN {t, n}",
                json!({"@w": WORK_ITEMS, "p": project}),
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| Some((r["t"].as_str()?.to_string(), r["n"].as_u64()?)))
            .collect())
    }

    /// Spec §12.2: a project stops claiming new work at its budget.
    pub async fn project_at_budget(&self, project: &str) -> Result<bool> {
        let budget = self.setting(project, "budget_usd").await?;
        let Some(b) = budget.as_f64() else {
            return Ok(false);
        };
        Ok(self.project_spend(project).await?["cost_usd"]
            .as_f64()
            .unwrap_or(0.0)
            >= b)
    }
}
