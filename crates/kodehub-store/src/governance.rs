//! Human approvals and notifications (spec §4.5, §7.3, §14.2).
//!
//! Humans approve only what is theirs: requirement changes, new contexts (and new containers
//! when `human_engagement = containers`), plans only under `human_engagement = plans`, and
//! production. There is no approvals collection (docs/ontology.md): the thing awaiting a human
//! carries the state —
//!
//! - a requirement change is a `requirement_versions` document in state `proposed` (a new
//!   requirement also has its node in state `proposed`);
//! - a new context or container is a node with `approval: pending`, not placed in the
//!   hierarchy (so not in any project version) until approved;
//! - a plan is a work item's proposal in state `planned` with `awaiting_human: true`;
//! - production is a deployment in state `awaiting_approval` (see `deploy.rs`).
//!
//! An approval key is `<collection>:<document key>` (`requirement_versions:shop.r1-r2-ab12`).
//! Notifications reach humans only for true external blockers (a revoked key, a missing
//! permission, a retired service), clearly marked as unrelated to the code.

use kodehub_core::model::now_iso;
use serde_json::{json, Value};

use crate::{Result, Store, StoreError};

pub const NOTIFICATIONS: &str = "notifications";

fn approval_state(s: &str) -> &'static str {
    match s {
        "proposed" | "pending" => "pending",
        "accepted" | "approved" => "approved",
        _ => "rejected",
    }
}

impl Store {
    /// Record a proposal that needs a human decision. Returns the approval record.
    pub async fn request_approval(
        &self,
        kind: &str,
        proposal: Value,
        rationale: &str,
        by: &str,
    ) -> Result<Value> {
        match kind {
            "requirement_change" => {
                let extra = json!({"rationale": rationale, "proposal": proposal});
                let rv = match proposal["requirement"].as_str().filter(|r| !r.is_empty()) {
                    Some(r) => {
                        let cur = self.db.get("requirements", crate::key_of(r), None).await?;
                        let mut next = cur.clone();
                        if let Some(o) = proposal["patch"].as_object() {
                            for (k, v) in o {
                                next[k] = v.clone();
                            }
                        }
                        self.put_requirement_version_pub(
                            cur["_id"].as_str().unwrap_or_default(),
                            cur["revision"].as_i64().unwrap_or(1) + 1,
                            &next,
                            "proposed",
                            by,
                            extra,
                        )
                        .await?
                    }
                    None => {
                        let mut doc = proposal["requirement_doc"].clone();
                        doc["state"] = json!("proposed");
                        doc["created_by"] = json!(by);
                        let r = self.create_requirement(doc).await?;
                        let rid = r["_id"].as_str().unwrap_or_default().to_string();
                        let v = self
                            .aql(
                                "FOR v IN @@requirement_versions FILTER v.requirement == @r RETURN v",
                                json!({"r": rid}),
                            )
                            .await?
                            .into_iter()
                            .next()
                            .ok_or_else(|| StoreError::NotFound("requirement version".into()))?;
                        self.db
                            .patch(
                                "requirement_versions",
                                v["_key"].as_str().unwrap_or_default(),
                                &extra,
                                None,
                            )
                            .await?
                    }
                };
                self.approval_of(
                    "requirement_versions",
                    rv["_key"].as_str().unwrap_or_default(),
                )
                .await
            }
            "new_context" | "new_container" => {
                let mut n = proposal.clone();
                n["approval"] = json!("pending");
                n["approval_rationale"] = json!(rationale);
                n["requested_by"] = json!(by);
                let id = self.create_planned_node(&n).await?;
                let (c, k) = id.split_once('/').unwrap_or_default();
                self.approval_of(c, k).await
            }
            other => Err(StoreError::Invalid(format!(
                "unknown approval kind `{other}`"
            ))),
        }
    }

    /// One approval record from the thing awaiting it.
    async fn approval_of(&self, coll: &str, key: &str) -> Result<Value> {
        let d = self.db.get(coll, key, None).await?;
        Ok(match coll {
            "requirement_versions" => {
                let r = self
                    .get_node(d["requirement"].as_str().unwrap_or_default())
                    .await
                    .unwrap_or(Value::Null);
                json!({
                    "_key": format!("{coll}:{key}"), "kind": "requirement_change",
                    "state": approval_state(d["state"].as_str().unwrap_or("")),
                    "proposal": d.get("proposal").cloned().unwrap_or(json!({"content": d["content"]})),
                    "content": d["content"], "revision": d["revision"],
                    "requirement": d["requirement"], "title": r["title"],
                    "rationale": d["rationale"], "requested_by": d["created_by"],
                    "created_at": d["created_at"], "decided_by": d["decided_by"],
                    "decided_at": d["decided_at"], "note": d["note"],
                })
            }
            _ => json!({
                "_key": format!("{coll}:{key}"),
                "kind": if coll == "contexts" { "new_context" } else { "new_container" },
                "state": approval_state(d["approval"].as_str().unwrap_or("")),
                "proposal": {"level": crate::c4::level_of(&format!("{coll}/x")), "name": d["name"],
                             "description": d["description"], "parent": d["planned_parent"], "key": d["_key"]},
                "node": d["_id"], "rationale": d["approval_rationale"],
                "requested_by": d["requested_by"], "created_at": d["created_at"],
                "decided_by": d["decided_by"], "decided_at": d["decided_at"], "note": d["note"],
            }),
        })
    }

    /// Pending (or decided) approvals, newest first.
    pub async fn list_approvals(&self, state: Option<&str>) -> Result<Vec<Value>> {
        let rows = self
            .aql(
                "FOR x IN UNION(
                    (FOR v IN @@requirement_versions FILTER v.state == 'proposed'
                       OR (v.decided_at != null AND v.proposal != null)
                       RETURN {c: 'requirement_versions', k: v._key, at: v.created_at}),
                    (FOR n IN @@contexts FILTER n.approval != null RETURN {c: 'contexts', k: n._key, at: n.created_at}),
                    (FOR n IN @@containers FILTER n.approval != null RETURN {c: 'containers', k: n._key, at: n.created_at}))
                   SORT x.at DESC LIMIT 200 RETURN x",
                json!({}),
            )
            .await?;
        let mut out = Vec::new();
        for r in rows {
            let a = self
                .approval_of(
                    r["c"].as_str().unwrap_or_default(),
                    r["k"].as_str().unwrap_or_default(),
                )
                .await?;
            if state.is_none_or(|s| a["state"] == s) {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// A human decides. An approved requirement change becomes the requirement's accepted
    /// version; an approved context or container is placed in the hierarchy (and so in the
    /// current project version).
    pub async fn decide_approval(
        &self,
        key: &str,
        approve: bool,
        by: &str,
        note: &str,
    ) -> Result<Value> {
        let (coll, k) = key
            .split_once(':')
            .ok_or_else(|| StoreError::NotFound(format!("approval {key}")))?;
        let a = self.approval_of(coll, k).await?;
        if a["state"] != "pending" {
            return Err(StoreError::Conflict(format!(
                "approval {key} is {}",
                a["state"]
            )));
        }
        let now = now_iso();
        match coll {
            "requirement_versions" => {
                let v = self.db.get(coll, k, None).await?;
                let rid = v["requirement"].as_str().unwrap_or_default().to_string();
                let r = self.get_node(&rid).await?;
                if approve {
                    if r["state"] == "proposed" {
                        // a new requirement: accept it and attach it where it was proposed
                        self.db
                            .patch(
                                "requirements",
                                crate::key_of(&rid),
                                &json!({"state": "accepted"}),
                                None,
                            )
                            .await?;
                        if let Some(n) = v["proposal"]["attach_to"].as_str() {
                            self.insert_edge("applies_to", &rid, n, json!({})).await?;
                        }
                    } else {
                        let patch = v["proposal"]["patch"].clone();
                        let patch = if patch.is_object() {
                            patch
                        } else {
                            v["content"].clone()
                        };
                        self.update_requirement(crate::key_of(&rid), &patch, by, true)
                            .await?;
                    }
                } else if r["state"] == "proposed" {
                    self.db
                        .patch(
                            "requirements",
                            crate::key_of(&rid),
                            &json!({"state": "rejected"}),
                            None,
                        )
                        .await?;
                }
                self.db
                    .patch(
                        coll,
                        k,
                        &json!({"state": if approve { "approved" } else { "rejected" },
                                "decided_by": by, "decided_at": now, "note": note}),
                        None,
                    )
                    .await?;
            }
            "contexts" | "containers" => {
                let n = self.db.get(coll, k, None).await?;
                self.db
                    .patch(
                        coll,
                        k,
                        &json!({"approval": if approve { "approved" } else { "rejected" },
                                "decided_by": by, "decided_at": now, "note": note}),
                        None,
                    )
                    .await?;
                if approve {
                    let parent = match n["planned_parent"].as_str() {
                        Some(p) => p.to_string(),
                        None => self.project_node()?,
                    };
                    let id = n["_id"].as_str().unwrap_or_default().to_string();
                    self.insert_edge("contains", &parent, &id, json!({}))
                        .await?;
                }
            }
            _ => return Err(StoreError::NotFound(format!("approval {key}"))),
        }
        self.approval_of(coll, k).await
    }

    /// Create a planned C4 node under a parent (`{level, name, description, parent, key?}`).
    /// With `approval: pending` the node waits unplaced for a human.
    pub async fn create_planned_node(&self, n: &Value) -> Result<String> {
        let coll = match n["level"].as_str() {
            Some("context") => "contexts",
            Some("container") => "containers",
            Some("component") => "components",
            other => {
                return Err(StoreError::Invalid(format!(
                    "cannot plan a node at level {other:?}"
                )))
            }
        };
        let name = n["name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| StoreError::Invalid("a planned node needs a name".into()))?;
        let key = n["key"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| crate::slug(name));
        let id = self.db.pid(coll, crate::arango::local_key(&key))?;
        let parent = match n["parent"].as_str() {
            Some(p) if p.starts_with("philosophy/") || p == "project" => self.project_node()?,
            Some(p) => self.db.qid(p)?,
            None => self.project_node()?,
        };
        let pending = n["approval"] == "pending";
        if self.get_node(&id).await.is_err() {
            let mut doc = json!({"_key": key, "title": name, "name": name, "description": n["description"],
                                 "lifecycle": "planned", "planned_by": n["planned_by"],
                                 "planned_parent": parent});
            if pending {
                for f in ["approval", "approval_rationale", "requested_by"] {
                    doc[f] = n[f].clone();
                }
            }
            self.insert_node(coll, doc).await?;
        }
        if !pending && parent != id && self.get_node(&parent).await.is_ok() {
            self.insert_edge("contains", &parent, &id, json!({}))
                .await?;
        }
        Ok(id)
    }

    /// Tell humans about an external blocker (global, so admins see it across projects).
    pub async fn notify(
        &self,
        project: &str,
        kind: &str,
        message: &str,
        detail: Value,
    ) -> Result<Value> {
        self.g_insert(
            NOTIFICATIONS,
            &json!({"project": project, "kind": kind, "message": message, "detail": detail,
                    "unrelated_to_code": kind == "external_blocker",
                    "read": false, "created_at": now_iso()}),
        )
        .await
    }

    pub async fn notifications(
        &self,
        project: Option<&str>,
        unread_only: bool,
    ) -> Result<Vec<Value>> {
        self.g_aql(
            "FOR n IN @@n FILTER (@p == null OR n.project == @p) AND (@u == false OR n.read == false)
               SORT n.created_at DESC LIMIT 200 RETURN UNSET(n, '_rev')",
            json!({"@n": NOTIFICATIONS, "p": project, "u": unread_only}),
        )
        .await
    }

    pub async fn mark_notification_read(&self, key: &str) -> Result<()> {
        self.g_patch(NOTIFICATIONS, key, &json!({"read": true}))
            .await
            .map(|_| ())
    }
}
