//! Human approvals and notifications (spec §4.5, §7.3, §14.2).
//!
//! Humans approve only what is theirs: requirement changes, new contexts (and new containers
//! when `human_engagement = containers`), changeset plans only under `human_engagement = plans`,
//! and production. Agents propose; a pending `approvals` record waits for a human.
//! Notifications reach humans only for true external blockers (a revoked key, a missing
//! permission, a retired service), clearly marked as unrelated to the code.

use kodehub_core::model::now_iso;
use serde_json::{json, Value};

use crate::{Result, Store, StoreError};

pub const NOTIFICATIONS: &str = "kodehub_notifications";

impl Store {
    /// Record a proposal that needs a human decision (project-scoped).
    pub async fn request_approval(
        &self,
        kind: &str,
        proposal: Value,
        rationale: &str,
        by: &str,
    ) -> Result<Value> {
        if !matches!(
            kind,
            "requirement_change" | "new_context" | "new_container" | "plan"
        ) {
            return Err(StoreError::Invalid(format!(
                "unknown approval kind `{kind}`"
            )));
        }
        self.db
            .insert(
                "approvals",
                &json!({"kind": kind, "proposal": proposal, "rationale": rationale, "state": "pending",
                        "requested_by": by, "created_at": now_iso()}),
                None,
            )
            .await
    }

    pub async fn list_approvals(&self, state: Option<&str>) -> Result<Vec<Value>> {
        self.aql(
            "FOR a IN @@approvals FILTER @s == null OR a.state == @s SORT a.created_at DESC LIMIT 200 RETURN a",
            json!({"s": state}),
        )
        .await
    }

    /// A human decides. Approved proposals are applied: requirement changes update (or create)
    /// the requirement; new contexts / containers are created as planned nodes.
    pub async fn decide_approval(
        &self,
        key: &str,
        approve: bool,
        by: &str,
        note: &str,
    ) -> Result<Value> {
        let a = self.db.get("approvals", key, None).await?;
        if a["state"] != "pending" {
            return Err(StoreError::Conflict(format!(
                "approval {key} is {}",
                a["state"]
            )));
        }
        let mut applied = Value::Null;
        if approve {
            let p = &a["proposal"];
            applied = match a["kind"].as_str() {
                Some("requirement_change") => match p["requirement"].as_str() {
                    Some(r) if !r.is_empty() => {
                        let (d, _) = self.update_requirement(r, &p["patch"], by, true).await?;
                        json!({"requirement": d["_id"], "revision": d["revision"]})
                    }
                    _ => {
                        let d = self
                            .create_requirement(p["requirement_doc"].clone())
                            .await?;
                        if let Some(n) = p["attach_to"].as_str() {
                            self.insert_edge(
                                "satisfies",
                                n,
                                d["_id"].as_str().unwrap_or_default(),
                                json!({}),
                            )
                            .await?;
                        }
                        json!({"requirement": d["_id"]})
                    }
                },
                Some("new_context") | Some("new_container") => {
                    let made = self.create_planned_node(p).await?;
                    json!({"node": made})
                }
                _ => Value::Null,
            };
        }
        self.db
            .patch(
                "approvals",
                key,
                &json!({"state": if approve { "approved" } else { "rejected" }, "decided_by": by,
                        "decided_at": now_iso(), "note": note, "applied": applied}),
                None,
            )
            .await?;
        self.db.get("approvals", key, None).await
    }

    /// Create a planned C4 node under a parent (`{level, name, description, parent, key?}`).
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
        let id = format!("{coll}/{key}");
        if self.get_node(&id).await.is_err() {
            self.insert_node(
                coll,
                json!({"_key": key, "title": name, "name": name, "description": n["description"],
                       "lifecycle": "planned", "planned_by": n["planned_by"]}),
            )
            .await?;
        }
        let parent = n["parent"]
            .as_str()
            .unwrap_or(crate::c4::PHILOSOPHY_ID)
            .to_string();
        if parent != id && self.get_node(&parent).await.is_ok() {
            let k = crate::edge_key("contains", &parent, &id);
            self.db
                .insert_ignore(
                    "contains",
                    &json!({"_key": k, "_from": parent, "_to": id}),
                    None,
                )
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
