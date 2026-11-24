//! Declared intents, overlap and negotiation (spec §10).
//!
//! When an agent claims a work item it registers an intent: the nodes it plans to change and
//! how. The orchestrator compares it with the project's active intents by Jaccard overlap
//! (shared nodes / all nodes either touches):
//!
//! - overlap ≥ `overlap_absorb_threshold` → **absorb**: the newer item folds into the older
//!   (survivor) one; they complete together and count once for retries, priority and caps;
//! - 0 < overlap < threshold → **sequence**: the newer item is re-queued behind the older;
//! - either side asks for a **shared end state** → a negotiation record where both agree on the
//!   end state of the shared nodes before working; after `negotiation_rounds` without agreement
//!   an architect agent decides.
//!
//! Every decision is a `negotiations` record linking the two work items.

use std::collections::BTreeSet;

use kodehub_core::model::now_iso;
use serde_json::{json, Value};

use crate::queue::{NewWorkItem, PRIORITY_HIGH, WORK_ITEMS};
use crate::{Result, Store, StoreError};

pub fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union as f64
}

impl Store {
    /// The same store, bound to one project (for global workers acting on project data).
    pub fn for_project(&self, project: &str) -> Store {
        Store {
            db: self.db.scoped(project),
            write_lock: self.write_lock.clone(),
        }
    }

    /// Register an intent for a claimed work item and apply the overlap decision. Returns
    /// `{decision: proceed | absorbed | sequenced | negotiate, …}`.
    pub async fn register_intent(
        &self,
        work_item: &str,
        nodes: &[String],
        description: &str,
        shared_end_state: bool,
    ) -> Result<Value> {
        let w = self.work_item(work_item).await?;
        let project = w["project"].as_str().unwrap_or_default().to_string();
        let ps = self.for_project(&project);
        let mine: BTreeSet<String> = nodes.iter().cloned().collect();
        if mine.is_empty() {
            return Err(StoreError::Invalid(
                "an intent names the nodes it will change".into(),
            ));
        }
        let threshold = self
            .setting(&project, "overlap_absorb_threshold")
            .await?
            .as_f64()
            .unwrap_or(0.6);
        let intent = ps
            .db
            .insert(
                "intents",
                &json!({"_key": work_item, "work_item": work_item, "nodes": nodes,
                        "description": description, "shared_end_state": shared_end_state,
                        "state": "active", "created_at": now_iso()}),
                None,
            )
            .await
            .or_else(|e| {
                if e.is_unique_violation() {
                    Ok(json!({}))
                } else {
                    Err(e)
                }
            })?;
        let _ = intent;
        let others = ps
            .aql(
                "FOR i IN @@intents FILTER i.state == 'active' AND i.work_item != @w
                   SORT i.created_at RETURN i",
                json!({"w": work_item}),
            )
            .await?;
        for o in others {
            let theirs: BTreeSet<String> = o["nodes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let shared: Vec<String> = mine.intersection(&theirs).cloned().collect();
            if shared.is_empty() {
                continue;
            }
            let score = jaccard(&mine, &theirs);
            let other = o["work_item"].as_str().unwrap_or_default().to_string();
            let decision = if shared_end_state || o["shared_end_state"] == true {
                "negotiate"
            } else if score >= threshold {
                "absorbed"
            } else {
                "sequenced"
            };
            let neg = ps
                .db
                .insert(
                    "negotiations",
                    &json!({"items": [other, work_item], "survivor": other, "shared_nodes": shared,
                            "overlap": score, "threshold": threshold, "decision": decision,
                            "state": if decision == "negotiate" { "open" } else { "decided" },
                            "rounds": [], "created_at": now_iso()}),
                    None,
                )
                .await?;
            let neg_key = neg["_key"].as_str().unwrap_or_default().to_string();
            match decision {
                "absorbed" => {
                    // fold this item into the survivor: completed together, counted once
                    let surv = self.work_item(&other).await?;
                    let mut combined = surv["combined_with"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    combined.push(json!(work_item));
                    let mut task = surv["task"].clone();
                    if !task.is_object() {
                        task = json!({"original": task});
                    }
                    let mut absorbed = task["absorbed"].as_array().cloned().unwrap_or_default();
                    absorbed.push(
                        json!({"work_item": work_item, "task": w["task"], "target": w["target"]}),
                    );
                    task["absorbed"] = json!(absorbed);
                    let prio = surv["priority"]
                        .as_i64()
                        .unwrap_or(0)
                        .max(w["priority"].as_i64().unwrap_or(0));
                    self.g_patch(
                        WORK_ITEMS,
                        &other,
                        &json!({"combined_with": combined, "task": task, "priority": prio}),
                    )
                    .await?;
                    self.g_patch(
                        WORK_ITEMS,
                        work_item,
                        &json!({"state": "absorbed", "absorbed_into": other, "owner": null,
                                "lease_expires": null, "negotiation": neg_key, "updated_at": now_iso()}),
                    )
                    .await?;
                    ps.db
                        .patch("intents", work_item, &json!({"state": "absorbed"}), None)
                        .await?;
                    let mut nodes_s: BTreeSet<String> = theirs.clone();
                    nodes_s.extend(mine.iter().cloned());
                    ps.db
                        .patch(
                            "intents",
                            &other,
                            &json!({"nodes": nodes_s.into_iter().collect::<Vec<_>>()}),
                            None,
                        )
                        .await?;
                    return Ok(
                        json!({"decision": "absorbed", "into": other, "overlap": score, "negotiation": neg_key}),
                    );
                }
                "sequenced" => {
                    let owner = w["owner"].as_str().unwrap_or_default().to_string();
                    self.release_work_item(work_item, &owner, std::slice::from_ref(&other))
                        .await?;
                    ps.db
                        .patch(
                            "intents",
                            work_item,
                            &json!({"state": "waiting", "behind": other}),
                            None,
                        )
                        .await?;
                    return Ok(
                        json!({"decision": "sequenced", "after": other, "overlap": score, "negotiation": neg_key}),
                    );
                }
                _ => {
                    return Ok(
                        json!({"decision": "negotiate", "with": other, "overlap": score,
                                     "negotiation": neg_key, "shared_nodes": shared}),
                    );
                }
            }
        }
        Ok(json!({"decision": "proceed"}))
    }

    /// One negotiation round: a party proposes an end state for the shared nodes (or agrees to
    /// the other's). Agreement when both sides' latest proposals match or one accepts the other;
    /// after the round limit an architect work item decides (spec §10.3).
    pub async fn negotiate(
        &self,
        project: &str,
        negotiation: &str,
        work_item: &str,
        proposal: &Value,
        accept: bool,
    ) -> Result<Value> {
        let ps = self.for_project(project);
        let n = ps.db.get("negotiations", negotiation, None).await?;
        if n["state"] != "open" {
            return Ok(n);
        }
        let items: Vec<String> = n["items"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if !items.iter().any(|i| i == work_item) {
            return Err(StoreError::Invalid(format!(
                "{work_item} is not a party to {negotiation}"
            )));
        }
        let mut rounds = n["rounds"].as_array().cloned().unwrap_or_default();
        rounds.push(
            json!({"by": work_item, "proposal": proposal, "accept": accept, "at": now_iso()}),
        );
        let last_other = rounds
            .iter()
            .rev()
            .find(|r| r["by"] != json!(work_item))
            .map(|r| r["proposal"].clone());
        let agreed = match &last_other {
            Some(p) => accept || p == proposal,
            None => false,
        };
        let limit = self
            .setting(project, "negotiation_rounds")
            .await?
            .as_u64()
            .unwrap_or(3) as usize;
        let mut patch = json!({"rounds": rounds});
        if agreed {
            patch["state"] = json!("agreed");
            patch["end_state"] = if accept {
                last_other.unwrap_or(Value::Null)
            } else {
                proposal.clone()
            };
        } else if rounds.len() >= limit * 2 {
            let arch = self
                .enqueue(&NewWorkItem {
                    project: project.into(),
                    agent_type: "architect".into(),
                    kind: "resolve_deadlock".into(),
                    priority: PRIORITY_HIGH,
                    task: json!({"negotiation": negotiation, "items": items, "shared_nodes": n["shared_nodes"],
                                 "rounds": patch["rounds"],
                                 "note": "agents could not agree; decide the end state using the merged requirements and the philosophy document"}),
                    ..Default::default()
                })
                .await?;
            patch["state"] = json!("deadlocked");
            patch["architect"] = arch["_key"].clone();
        }
        ps.db
            .patch("negotiations", negotiation, &patch, None)
            .await?;
        ps.db.get("negotiations", negotiation, None).await
    }

    /// Close the intents of a finished work item (and of everything it absorbed).
    pub async fn close_intents(&self, project: &str, work_item: &str, state: &str) -> Result<()> {
        let ps = self.for_project(project);
        ps.aql(
            "FOR i IN @@intents FILTER i.work_item == @w AND i.state IN ['active', 'waiting']
               UPDATE i WITH {state: @s, closed_at: @now} IN @@intents",
            json!({"w": work_item, "s": state, "now": now_iso()}),
        )
        .await?;
        Ok(())
    }

    pub async fn list_negotiations(&self, project: &str) -> Result<Vec<Value>> {
        self.for_project(project)
            .aql(
                "FOR n IN @@negotiations SORT n.created_at DESC LIMIT 200 RETURN n",
                json!({}),
            )
            .await
    }

    pub async fn list_intents(&self, project: &str) -> Result<Vec<Value>> {
        self.for_project(project)
            .aql(
                "FOR i IN @@intents SORT i.created_at DESC LIMIT 200 RETURN i",
                json!({}),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_matches_the_spec_example() {
        let a: BTreeSet<String> = (0..20).map(|i| format!("n{i}")).collect();
        let b: BTreeSet<String> = (2..22).map(|i| format!("n{i}")).collect();
        let j = jaccard(&a, &b);
        assert!((j - 18.0 / 22.0).abs() < 1e-9, "{j}");
        assert!(j >= 0.6);
        let c: BTreeSet<String> = ["x".to_string()].into();
        assert_eq!(jaccard(&a, &c), 0.0);
    }
}
