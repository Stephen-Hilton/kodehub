//! Schedules and triggers (spec §3.3, §5.7, §11.3, §11.5).
//!
//! A schedule fires on a timer: when `next_due` passes, a scheduler replica claims it with the
//! same claim-and-lease protocol as work items, turns it into a `test_run` work item, and sets
//! `next_due` from the time it actually ran, so the plan balances itself. Triggers fire on
//! events (a version landed, *Test now*): a trigger record is claimed exactly once.
//!
//! Default cadence (application defaults, layered settings): code on landing, components every
//! 2 h, containers 4 h, contexts 12 h, the whole project 24 h. Runs are spread over the
//! interval by a stable offset per node. Only nodes that are ready for test or live are
//! scheduled (spec §14.4); the daily project run is always kept.

use kodehub_core::model::now_iso;
use serde_json::{json, Value};

use crate::queue::{NewWorkItem, PRIORITY_HIGH, PRIORITY_NORMAL};
use crate::{Result, Store, StoreError};

pub const SCHEDULES: &str = "kodehub_schedules";
pub const TRIGGERS: &str = "kodehub_triggers";

fn ts(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// Stable offset in [0, interval) so equal intervals do not all fire on the hour (spec §11.5).
pub fn spread_offset(key: &str, interval_secs: i64) -> i64 {
    if interval_secs <= 0 {
        return 0;
    }
    let h = blake3::hash(key.as_bytes());
    let n = u64::from_le_bytes(h.as_bytes()[..8].try_into().unwrap());
    (n % interval_secs as u64) as i64
}

fn schedule_key(project: &str, target: &str) -> String {
    blake3::hash(format!("{project}|{target}").as_bytes()).to_hex()[..24].to_string()
}

impl Store {
    /// Create or update a timer schedule for a test target.
    pub async fn upsert_schedule(
        &self,
        project: &str,
        target: &str,
        level: &str,
        interval_secs: i64,
        source: &str,
    ) -> Result<Value> {
        self.upsert_action_schedule(
            project,
            target,
            level,
            interval_secs,
            source,
            "test_run",
            Value::Null,
        )
        .await
    }

    /// A timer schedule for any deterministic action: `test_run`, `backup`, `git_export`.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_action_schedule(
        &self,
        project: &str,
        target: &str,
        level: &str,
        interval_secs: i64,
        source: &str,
        action: &str,
        task: Value,
    ) -> Result<Value> {
        if interval_secs < 60 {
            return Err(StoreError::Invalid(
                "schedules run at most once a minute".into(),
            ));
        }
        let key = schedule_key(project, target);
        let existing = self.g_get(SCHEDULES, &key).await?;
        let now = chrono::Utc::now();
        let next_due = match &existing {
            // keep the phase when only the interval changes: next run = last run + interval
            Some(e) => e["last_run"]
                .as_str()
                .and_then(parse)
                .map(|l| l + chrono::Duration::seconds(interval_secs))
                .unwrap_or_else(|| {
                    now + chrono::Duration::seconds(spread_offset(&key, interval_secs))
                }),
            None => now + chrono::Duration::seconds(spread_offset(&key, interval_secs)),
        };
        let doc = json!({
            "_key": key, "project": project, "target": target, "level": level,
            "interval_secs": interval_secs, "next_due": ts(next_due),
            "last_run": existing.as_ref().map(|e| e["last_run"].clone()).unwrap_or(Value::Null),
            "owner": null, "lease_expires": null, "enabled": true, "source": source,
            "always": level == "project", "action": action, "task": task,
            "updated_at": now_iso(),
        });
        self.db
            .db_raw(
                reqwest::Method::POST,
                &format!("/_api/document/{SCHEDULES}?overwriteMode=replace&returnNew=true"),
                Some(&doc),
            )
            .await
            .map(|v| v["new"].clone())
    }

    pub async fn list_schedules(&self, project: &str) -> Result<Vec<Value>> {
        self.g_aql(
            "FOR s IN @@s FILTER s.project == @p SORT s.level, s.target RETURN UNSET(s, '_rev')",
            json!({"@s": SCHEDULES, "p": project}),
        )
        .await
    }

    pub async fn set_schedule_enabled(
        &self,
        project: &str,
        key: &str,
        enabled: bool,
    ) -> Result<Value> {
        let s = self
            .g_get(SCHEDULES, key)
            .await?
            .filter(|s| s["project"] == project)
            .ok_or_else(|| StoreError::NotFound(format!("schedule {key}")))?;
        if !enabled && s["always"] == true {
            return Err(StoreError::Invalid(
                "the daily full-project run is always kept (spec §11.3)".into(),
            ));
        }
        self.g_patch(SCHEDULES, key, &json!({"enabled": enabled}))
            .await
    }

    /// (Re)build a project's default cadence from the `test_cadence` setting: one schedule per
    /// component / container / context that is ready for test or live, and the project run.
    /// Schedules for nodes that are no longer eligible are disabled (the project run never).
    pub async fn ensure_default_schedules(&self, project: &str) -> Result<Vec<Value>> {
        let cad = self.setting(project, "test_cadence").await?;
        let hours = |k: &str, d: f64| cad[k].as_f64().unwrap_or(d);
        let levels = [
            ("components", "component", hours("component_hours", 2.0)),
            ("containers", "container", hours("container_hours", 4.0)),
            ("contexts", "context", hours("context_hours", 12.0)),
        ];
        let lifecycle =
            crate::with_project(project.to_string(), async { self.node_lifecycle().await }).await?;
        let mut eligible = Vec::new();
        for (coll, level, h) in levels {
            for (id, l) in &lifecycle {
                if !id.starts_with(&format!("{coll}/")) {
                    continue;
                }
                let st = l["state"].as_str().unwrap_or("planned");
                if matches!(st, "ready_for_test" | "live") {
                    eligible.push((id.clone(), level, (h * 3600.0) as i64));
                }
            }
        }
        let project_secs = (hours("project_hours", 24.0) * 3600.0).min(24.0 * 3600.0) as i64;
        let mut out = vec![
            self.upsert_schedule(project, "project", "project", project_secs, "default")
                .await?,
        ];
        // off-site backup of the graph (spec §3.4) and the scheduled Git mirror (spec §13.2)
        let backup_h = self
            .setting(project, "db_backup_hours")
            .await?
            .as_f64()
            .unwrap_or(24.0);
        if backup_h > 0.0 {
            out.push(
                self.upsert_action_schedule(
                    project,
                    "backup",
                    "backup",
                    ((backup_h * 3600.0) as i64).max(3600),
                    "default",
                    "backup",
                    Value::Null,
                )
                .await?,
            );
        }
        let gx = self.setting(project, "git_export").await?;
        if let Some(url) = gx["repo_url"].as_str() {
            let every = gx["every_hours"].as_f64().unwrap_or(24.0);
            out.push(
                self.upsert_action_schedule(
                    project,
                    "git_export",
                    "git_export",
                    ((every * 3600.0) as i64).max(3600),
                    "default",
                    "git_export",
                    json!({"repo_url": url, "branch": gx["branch"].as_str().unwrap_or("main"),
                           "message": "Kodehub scheduled export"}),
                )
                .await?,
            );
        }
        let mut keep: std::collections::BTreeSet<String> =
            eligible.iter().map(|(id, _, _)| id.clone()).collect();
        keep.insert("backup".into());
        keep.insert("git_export".into());
        for (id, level, secs) in eligible {
            out.push(
                self.upsert_schedule(project, &id, level, secs, "default")
                    .await?,
            );
        }
        // nodes that fell back to planned / in build are not tested
        self.g_aql(
            "FOR s IN @@s FILTER s.project == @p AND s.always != true AND s.target NOT IN @keep
               UPDATE s WITH {enabled: false, disabled_reason: 'not ready for test'} IN @@s",
            json!({"@s": SCHEDULES, "p": project, "keep": keep.into_iter().collect::<Vec<_>>()}),
        )
        .await?;
        Ok(out)
    }

    /// Claim due schedules (claim-and-lease) and turn each into a `test_run` work item. Safe to
    /// run on several scheduler replicas at once: each due run fires exactly once.
    pub async fn fire_due_schedules(&self, owner: &str, lease_secs: i64) -> Result<Vec<Value>> {
        let now = now_iso();
        let due = self
            .g_aql(
                "FOR s IN @@s FILTER s.enabled == true AND s.next_due <= @now
                   AND (s.owner == null OR s.lease_expires < @now)
                   SORT s.next_due LIMIT 100 RETURN s",
                json!({"@s": SCHEDULES, "now": now}),
            )
            .await?;
        let mut fired = Vec::new();
        for s in due {
            let claimed = self
                .g_aql(
                    "LET d = DOCUMENT(@@s, @k) FILTER d._rev == @rev AND d.next_due <= @now
                     UPDATE d WITH {owner: @o, lease_expires: @until} IN @@s OPTIONS {ignoreRevs: false}
                     RETURN NEW",
                    json!({"@s": SCHEDULES, "k": s["_key"], "rev": s["_rev"], "now": now, "o": owner,
                           "until": ts(chrono::Utc::now() + chrono::Duration::seconds(lease_secs))}),
                )
                .await;
            let claimed = match claimed {
                Ok(r) => r.into_iter().next(),
                Err(e) if crate::queue::is_conflict(&e) => None,
                Err(e) => return Err(e),
            };
            let Some(s) = claimed else { continue };
            let project = s["project"].as_str().unwrap_or_default().to_string();
            let action = s["action"].as_str().unwrap_or("test_run").to_string();
            let task = if action == "test_run" {
                json!({"scope": s["target"], "schedule": s["_key"], "formal": true})
            } else {
                s["task"].clone()
            };
            let item = self
                .enqueue(&NewWorkItem {
                    project,
                    agent_type: action,
                    kind: format!("scheduled_{}", s["level"].as_str().unwrap_or("node")),
                    target: s["target"].as_str().map(str::to_string),
                    priority: PRIORITY_NORMAL,
                    task,
                    requested_by: Some(format!("scheduler:{owner}")),
                    ..Default::default()
                })
                .await?;
            let ran = chrono::Utc::now();
            let interval = s["interval_secs"].as_i64().unwrap_or(3600);
            self.g_patch(
                SCHEDULES,
                s["_key"].as_str().unwrap_or_default(),
                &json!({"owner": null, "lease_expires": null, "last_run": ts(ran),
                        "next_due": ts(ran + chrono::Duration::seconds(interval)),
                        "last_work_item": item["_key"], "runs": s["runs"].as_i64().unwrap_or(0) + 1}),
            )
            .await?;
            fired.push(item);
        }
        Ok(fired)
    }

    /// Record an event trigger (version landed, Test now). Fired once by a scheduler replica.
    pub async fn trigger(
        &self,
        project: &str,
        kind: &str,
        target: &str,
        by: &str,
    ) -> Result<Value> {
        self.g_insert(
            TRIGGERS,
            &json!({"project": project, "kind": kind, "target": target, "by": by,
                    "state": "queued", "owner": null, "created_at": now_iso()}),
        )
        .await
    }

    /// Claim and fire queued triggers: each becomes a high-priority `test_run` work item.
    pub async fn fire_triggers(&self, owner: &str) -> Result<Vec<Value>> {
        let queued = self
            .g_aql(
                "FOR t IN @@t FILTER t.state == 'queued' SORT t.created_at LIMIT 100 RETURN t",
                json!({"@t": TRIGGERS}),
            )
            .await?;
        let mut fired = Vec::new();
        for t in queued {
            let won = self
                .g_aql(
                    "LET d = DOCUMENT(@@t, @k) FILTER d._rev == @rev AND d.state == 'queued'
                     UPDATE d WITH {state: 'fired', owner: @o, fired_at: @now} IN @@t OPTIONS {ignoreRevs: false}
                     RETURN NEW",
                    json!({"@t": TRIGGERS, "k": t["_key"], "rev": t["_rev"], "o": owner, "now": now_iso()}),
                )
                .await;
            let won = match won {
                Ok(r) => !r.is_empty(),
                Err(e) if crate::queue::is_conflict(&e) => false,
                Err(e) => return Err(e),
            };
            if !won {
                continue;
            }
            let item = self
                .enqueue(&NewWorkItem {
                    project: t["project"].as_str().unwrap_or_default().into(),
                    agent_type: "test_run".into(),
                    kind: t["kind"].as_str().unwrap_or("trigger").into(),
                    target: t["target"].as_str().map(str::to_string),
                    priority: PRIORITY_HIGH,
                    task: json!({"scope": t["target"], "trigger": t["_key"], "formal": true}),
                    requested_by: t["by"].as_str().map(str::to_string),
                    ..Default::default()
                })
                .await?;
            self.g_patch(
                TRIGGERS,
                t["_key"].as_str().unwrap_or_default(),
                &json!({"work_item": item["_key"]}),
            )
            .await?;
            fired.push(item);
        }
        Ok(fired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_spread_and_are_stable() {
        let a = spread_offset("a", 7200);
        assert_eq!(a, spread_offset("a", 7200));
        assert!((0..7200).contains(&a));
        let distinct: std::collections::BTreeSet<i64> = (0..50)
            .map(|i| spread_offset(&format!("n{i}"), 7200) / 600)
            .collect();
        assert!(
            distinct.len() > 6,
            "spread over the interval, not one hour mark"
        );
    }
}
