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
//! Every decision is an `agent_chats` thread (`kind: negotiation`) whose messages refer to both
//! work items; the intent itself is the work item's `intent` plus `intends_changes` edges.

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
            trees: self.trees.clone(),
        }
    }

    /// Register an intent for a claimed work item and apply the overlap decision. Returns
    /// `{decision: proceed | absorbed | sequenced | negotiate, …}`.
    ///
    /// The intent lives on the work item (`intent`) with an `intends_changes` edge to every node
    /// it names (docs/ontology.md); overlap is computed from those edges' targets.
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
        let nodes: Vec<String> = nodes.iter().map(|n| ps.db.qid(n)).collect::<Result<_>>()?;
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
        if !w["intent"].is_object() {
            self.g_patch(
                WORK_ITEMS,
                work_item,
                &json!({"intent": {"nodes": nodes, "description": description,
                                   "shared_end_state": shared_end_state, "state": "active",
                                   "created_at": now_iso()}}),
            )
            .await?;
            ps.write_intent_edges(work_item, &nodes).await?;
        }
        let others = self
            .g_aql(
                "FOR w IN @@w FILTER w.project == @p AND w._key != @k AND w.intent.state == 'active'
                   SORT w.intent.created_at RETURN w",
                json!({"@w": WORK_ITEMS, "p": project, "k": work_item}),
            )
            .await?;
        for o in others {
            let theirs: BTreeSet<String> = o["intent"]["nodes"]
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
            let other = o["_key"].as_str().unwrap_or_default().to_string();
            let decision = if shared_end_state || o["intent"]["shared_end_state"] == true {
                "negotiate"
            } else if score >= threshold {
                "absorbed"
            } else {
                "sequenced"
            };
            let chat = ps
                .open_negotiation(&other, work_item, &shared, score, threshold, decision)
                .await?;
            let neg_key = chat["_key"].as_str().unwrap_or_default().to_string();
            match decision {
                "absorbed" => {
                    // fold this item into the survivor: completed together, counted once
                    let mut combined = o["combined_with"].as_array().cloned().unwrap_or_default();
                    combined.push(json!(work_item));
                    let mut task = o["task"].clone();
                    if !task.is_object() {
                        task = json!({"original": task});
                    }
                    let mut absorbed = task["absorbed"].as_array().cloned().unwrap_or_default();
                    absorbed.push(
                        json!({"work_item": work_item, "task": w["task"], "target": w["target"]}),
                    );
                    task["absorbed"] = json!(absorbed);
                    let prio = o["priority"]
                        .as_i64()
                        .unwrap_or(0)
                        .max(w["priority"].as_i64().unwrap_or(0));
                    let mut nodes_s: BTreeSet<String> = theirs.clone();
                    nodes_s.extend(mine.iter().cloned());
                    let mut intent = o["intent"].clone();
                    intent["nodes"] = json!(nodes_s.iter().collect::<Vec<_>>());
                    self.g_patch(
                        WORK_ITEMS,
                        &other,
                        &json!({"combined_with": combined, "task": task, "priority": prio,
                                "intent": intent}),
                    )
                    .await?;
                    ps.write_intent_edges(&other, &nodes_s.into_iter().collect::<Vec<_>>())
                        .await?;
                    let mut my_intent = w["intent"].clone();
                    if !my_intent.is_object() {
                        my_intent = json!({"nodes": nodes, "description": description});
                    }
                    my_intent["state"] = json!("absorbed");
                    self.g_patch(
                        WORK_ITEMS,
                        work_item,
                        &json!({"state": "absorbed", "absorbed_into": other, "owner": null,
                                "lease_expires": null, "negotiation": neg_key, "intent": my_intent,
                                "updated_at": now_iso()}),
                    )
                    .await?;
                    return Ok(
                        json!({"decision": "absorbed", "into": other, "overlap": score, "negotiation": neg_key}),
                    );
                }
                "sequenced" => {
                    let owner = self.work_item(work_item).await?["owner"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    self.release_work_item(work_item, &owner, std::slice::from_ref(&other))
                        .await?;
                    let mut my_intent = self.work_item(work_item).await?["intent"].clone();
                    my_intent["state"] = json!("waiting");
                    my_intent["behind"] = json!(other);
                    self.g_patch(WORK_ITEMS, work_item, &json!({"intent": my_intent}))
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

    /// `intends_changes` edges from a work item to the nodes its intent names.
    async fn write_intent_edges(&self, work_item: &str, nodes: &[String]) -> Result<()> {
        let from = format!("{WORK_ITEMS}/{work_item}");
        for n in nodes {
            if self.get_node(n).await.is_err() {
                continue; // a node that does not exist yet (planned by this work) has no edge
            }
            self.db
                .insert_ignore(
                    "intends_changes",
                    &json!({"_key": crate::edge_key("intends_changes", &from, n),
                            "_from": from, "_to": n, "created_at": now_iso()}),
                    None,
                )
                .await?;
        }
        Ok(())
    }

    /// A negotiation is an agent chat between two work items (the survivor first), opened with
    /// a message that refers to both and to the shared nodes.
    async fn open_negotiation(
        &self,
        survivor: &str,
        other: &str,
        shared: &[String],
        overlap: f64,
        threshold: f64,
        decision: &str,
    ) -> Result<Value> {
        let chat = self
            .db
            .insert(
                "agent_chats",
                &json!({"kind": "negotiation", "items": [survivor, other], "survivor": survivor,
                        "shared_nodes": shared, "overlap": overlap, "threshold": threshold,
                        "decision": decision,
                        "state": if decision == "negotiate" { "open" } else { "decided" },
                        "title": format!("Overlap between {survivor} and {other}"),
                        "created_at": now_iso()}),
                None,
            )
            .await?;
        self.own(chat["_id"].as_str().unwrap_or_default()).await?;
        let mut refs: Vec<String> = vec![
            format!("{WORK_ITEMS}/{survivor}"),
            format!("{WORK_ITEMS}/{other}"),
        ];
        refs.extend(shared.iter().cloned());
        self.chat_message(
            chat["_id"].as_str().unwrap_or_default(),
            "orchestrator",
            &format!(
                "{other} overlaps {survivor} on {} node(s) (Jaccard {overlap:.2}, threshold {threshold}): {decision}.",
                shared.len()
            ),
            json!({"decision": decision}),
            &refs,
        )
        .await?;
        Ok(chat)
    }

    /// Append a message to an agent chat. `by` is a work item key, an agent type or a user id;
    /// `refers_to` are context pointers (any node or work item).
    pub async fn chat_message(
        &self,
        chat_id: &str,
        by: &str,
        text: &str,
        data: Value,
        refers_to: &[String],
    ) -> Result<Value> {
        let m = self
            .db
            .insert(
                "agent_chat_msgs",
                &json!({"chat": chat_id, "by": by, "text": text, "data": data,
                        "created_at": now_iso()}),
                None,
            )
            .await?;
        let mid = m["_id"].as_str().unwrap_or_default().to_string();
        self.db
            .insert(
                "has_message",
                &json!({"_key": crate::edge_key("has_message", chat_id, &mid),
                        "_from": chat_id, "_to": mid}),
                None,
            )
            .await?;
        // the author: a user (global) or an agent type of this project
        let author = if by.starts_with("users/") || by.starts_with("remote_agents/") {
            Some(by.to_string())
        } else {
            let k = self.db.pkey(&crate::agents::agent_key(by))?;
            self.db
                .get("agents", &k, None)
                .await
                .ok()
                .map(|_| format!("agents/{k}"))
        };
        if let Some(a) = author {
            self.db
                .insert_ignore(
                    "authored",
                    &json!({"_key": crate::edge_key("authored", &a, &mid), "_from": a, "_to": mid}),
                    None,
                )
                .await?;
        }
        for r in refers_to {
            let r = self.db.qid(r)?;
            self.db
                .insert_ignore(
                    "refers_to",
                    &json!({"_key": crate::edge_key("refers_to", &mid, &r), "_from": mid, "_to": r}),
                    None,
                )
                .await?;
        }
        Ok(m)
    }

    /// A chat with its messages, oldest first.
    pub async fn get_chat(&self, chat: &str) -> Result<Value> {
        let c = self
            .db
            .get("agent_chats", crate::key_of(chat), None)
            .await?;
        let msgs = self
            .aql(
                "FOR e IN @@has_message FILTER e._from == @c LET m = DOCUMENT(e._to)
                   SORT m.created_at
                   RETURN MERGE(m, {refers_to: (FOR r IN @@refers_to FILTER r._from == m._id RETURN r._to)})",
                json!({"c": c["_id"]}),
            )
            .await?;
        let mut v = c;
        v["messages"] = json!(msgs);
        Ok(v)
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
        let n = ps.get_chat(negotiation).await?;
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
        let chat_id = n["_id"].as_str().unwrap_or_default().to_string();
        ps.chat_message(
            &chat_id,
            work_item,
            if accept {
                "accepts the other side's end state"
            } else {
                "proposes an end state"
            },
            json!({"round": true, "proposal": proposal, "accept": accept}),
            &[format!("{WORK_ITEMS}/{work_item}")],
        )
        .await?;
        let rounds: Vec<Value> = ps.get_chat(negotiation).await?["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m["data"]["round"] == true)
            .collect();
        let last_other = rounds
            .iter()
            .rev()
            .find(|r| r["by"] != json!(work_item))
            .map(|r| r["data"]["proposal"].clone());
        let agreed = match &last_other {
            Some(p) => accept || p == proposal,
            None => false,
        };
        let limit = self
            .setting(project, "negotiation_rounds")
            .await?
            .as_u64()
            .unwrap_or(3) as usize;
        let mut patch = json!({"rounds": rounds.len()});
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
                                 "rounds": rounds.iter().map(|r| json!({"by": r["by"], "proposal": r["data"]["proposal"], "accept": r["data"]["accept"]})).collect::<Vec<_>>(),
                                 "note": "agents could not agree; decide the end state using the merged requirements and the philosophy document"}),
                    ..Default::default()
                })
                .await?;
            patch["state"] = json!("deadlocked");
            patch["architect"] = arch["_key"].clone();
        }
        ps.db
            .patch("agent_chats", crate::key_of(negotiation), &patch, None)
            .await?;
        let mut out = ps.get_chat(negotiation).await?;
        out["rounds"] = json!(rounds);
        Ok(out)
    }

    /// Close the intents of a finished work item (and of everything it absorbed).
    pub async fn close_intents(&self, project: &str, work_item: &str, state: &str) -> Result<()> {
        let w = self.work_item(work_item).await?;
        if w["intent"].is_object() && w["project"] == json!(project) {
            let mut intent = w["intent"].clone();
            if matches!(intent["state"].as_str(), Some("active") | Some("waiting")) {
                intent["state"] = json!(state);
                intent["closed_at"] = json!(now_iso());
                self.g_patch(WORK_ITEMS, work_item, &json!({"intent": intent}))
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn list_negotiations(&self, project: &str) -> Result<Vec<Value>> {
        self.for_project(project)
            .aql(
                "FOR c IN @@agent_chats FILTER c.kind == 'negotiation'
                   SORT c.created_at DESC LIMIT 200 RETURN c",
                json!({}),
            )
            .await
    }

    /// Intents of the project's work items, newest first.
    pub async fn list_intents(&self, project: &str) -> Result<Vec<Value>> {
        self.g_aql(
            "FOR w IN @@w FILTER w.project == @p AND w.intent != null
               SORT w.intent.created_at DESC LIMIT 200
               RETURN MERGE(w.intent, {work_item: w._key, agent_type: w.agent_type, kind: w.kind})",
            json!({"@w": WORK_ITEMS, "p": project}),
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
