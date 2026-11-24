//! Layered settings (spec §11.2–11.3).
//!
//! One set of settings applies at several scopes; the lowest scope wins:
//!
//! - user-owned project: application default → app override → owner user → project
//! - org-owned project:  application default → app override → org → project
//!   (an org's undefined setting falls through to the app default, never to contributing users)
//!
//! Per-actor settings (credentials are handled the same way in `credentials.rs`) come from the
//! acting user. Who may change a scope is governed by roles: the system admin for `app`, the user
//! for `user:<u>`, org admins for `org:<o>`, project admins for `project:<p>`.

use std::collections::BTreeMap;

use kodehub_core::model::now_iso;
use reqwest::Method;
use serde_json::{json, Value};

use crate::identity::{Owner, Permission, Principal};
use crate::{Result, Store, StoreError};

pub const SETTINGS: &str = "kodehub_settings";

/// A known setting: key, application default, per-actor flag, description.
pub struct SettingDef {
    pub key: &'static str,
    pub default: fn() -> Value,
    pub per_actor: bool,
    pub description: &'static str,
}

/// Every setting the system knows, with its deliberately conservative default.
pub fn registry() -> Vec<SettingDef> {
    vec![
        SettingDef {
            key: "retry_limit",
            default: || json!(3),
            per_actor: false,
            description: "Rework cycles before a work item escalates to an architect agent.",
        },
        SettingDef {
            key: "blind_review",
            default: || json!(true),
            per_actor: false,
            description: "Require a passing blind review before a changeset promotes.",
        },
        SettingDef {
            key: "test_cadence",
            default: || {
                json!({"code": "on_landing", "component_hours": 2, "container_hours": 4,
                       "context_hours": 12, "project_hours": 24})
            },
            per_actor: false,
            description:
                "How often each C4 level is retested. The daily project run is always kept.",
        },
        SettingDef {
            key: "test_budget_pct",
            default: || json!(30),
            per_actor: false,
            description:
                "Largest share of cluster capacity tests may use (the daily run is exempt).",
        },
        SettingDef {
            key: "test_levels_up",
            default: || json!(1),
            per_actor: false,
            description: "How many C4 levels above a changed node formal tests run.",
        },
        SettingDef {
            key: "agent_caps",
            default: || {
                json!({"coding": 3, "test_author": 2, "review": 2, "planning": 2,
                       "architect": 1, "import": 1, "design": 1, "sysadmin": 1})
            },
            per_actor: false,
            description: "Concurrency cap per agent type (the sysadmin agent tunes these).",
        },
        SettingDef {
            key: "agent_caps_hard_limit",
            default: || json!(null),
            per_actor: false,
            description: "Human hard limit on agents of any type (null = no limit).",
        },
        SettingDef {
            key: "human_engagement",
            default: || json!("contexts"),
            per_actor: false,
            description: "What needs human approval: `contexts` (new contexts and requirement \
                          changes), `containers` (also new containers), `plans` (also every \
                          changeset plan, the v0.05 flow), or `none`.",
        },
        SettingDef {
            key: "overlap_absorb_threshold",
            default: || json!(0.6),
            per_actor: false,
            description:
                "Jaccard overlap of two intents above which one work item absorbs the other.",
        },
        SettingDef {
            key: "negotiation_rounds",
            default: || json!(3),
            per_actor: false,
            description: "Negotiation rounds before an architect agent decides.",
        },
        SettingDef {
            key: "parent_ready_requires_children",
            default: || json!(true),
            per_actor: false,
            description:
                "A parent node is not ready for test while a child is planned or in build.",
        },
        SettingDef {
            key: "budget_usd",
            default: || json!(null),
            per_actor: false,
            description: "Model spend budget per project (null = unlimited).",
        },
        SettingDef {
            key: "environments",
            default: || {
                json!([
                    {"name": "dev", "max_releases": null},
                    {"name": "test", "max_releases": null},
                    {"name": "qa", "max_releases": null},
                    {"name": "preprod", "max_releases": 1},
                    {"name": "prod", "max_releases": 1, "requires_human_approval": true},
                ])
            },
            per_actor: false,
            description: "Ordered environments, lowest first, with their release limits.",
        },
        SettingDef {
            key: "lease_secs",
            default: || json!(900),
            per_actor: false,
            description: "Lease length for claimed work items and schedules.",
        },
        SettingDef {
            key: "agent_backend",
            default: || json!("claude_code"),
            per_actor: false,
            description: "Agent runtime: `claude_code`, `api` or `recorded`.",
        },
        SettingDef {
            key: "session_ttl_hours",
            default: || json!(168),
            per_actor: true,
            description: "How long a sign-in session lasts.",
        },
        SettingDef {
            key: "token_max_lifetime_days",
            default: || json!(90),
            per_actor: true,
            description: "Longest lifetime of a personal access token.",
        },
        SettingDef {
            key: "kickoff",
            default: || json!(true),
            per_actor: false,
            description: "Creating a project enqueues the kickoff chain (architect → planning → tests → code).",
        },
        SettingDef {
            key: "use_case_check",
            default: || json!(true),
            per_actor: false,
            description: "An architect agent checks a use case against the live ones before it goes live.",
        },
        SettingDef {
            key: "agent_model",
            default: || json!("claude-sonnet-5-5"),
            per_actor: false,
            description: "Model for agents (per-type overrides in agent types).",
        },
        SettingDef {
            key: "db_backup_hours",
            default: || json!(24),
            per_actor: false,
            description: "How often each project's graph is backed up into storage (0 = never).",
        },
        SettingDef {
            key: "git_export",
            default: || json!(null),
            per_actor: false,
            description: "Scheduled Git mirror: {repo_url, branch, every_hours} (null = on demand only).",
        },
        SettingDef {
            key: "open_registration",
            default: || json!(true),
            per_actor: false,
            description: "Anyone may create an account (app scope only).",
        },
    ]
}

pub fn definition(key: &str) -> Option<SettingDef> {
    registry().into_iter().find(|d| d.key == key)
}

fn setting_key(scope: &str, key: &str) -> String {
    blake3::hash(format!("{scope}|{key}").as_bytes()).to_hex()[..32].to_string()
}

/// Parse and validate a scope string.
pub fn parse_scope(scope: &str) -> Result<(&str, &str)> {
    if scope == "app" {
        return Ok(("app", ""));
    }
    match scope.split_once(':') {
        Some((k @ ("user" | "org" | "project"), id)) if !id.is_empty() => Ok((k, id)),
        _ => Err(StoreError::Invalid(format!(
            "scope must be app, user:<id>, org:<id> or project:<id> (got `{scope}`)"
        ))),
    }
}

impl Store {
    async fn scope_values(&self, scope: &str) -> Result<BTreeMap<String, Value>> {
        let rows = self
            .g_aql(
                "FOR s IN @@s FILTER s.scope == @scope RETURN s",
                json!({"@s": SETTINGS, "scope": scope}),
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| Some((r["key"].as_str()?.to_string(), r["value"].clone())))
            .collect())
    }

    /// The scope chain for a project, lowest last.
    pub async fn project_scope_chain(&self, project: &str) -> Result<Vec<String>> {
        let mut chain = vec!["app".to_string()];
        match self.project_owner(project).await? {
            Some(Owner::User(u)) => chain.push(format!("user:{u}")),
            Some(Owner::Org(o)) => chain.push(format!("org:{o}")),
            None => {}
        }
        chain.push(format!("project:{project}"));
        Ok(chain)
    }

    /// Resolve every setting for a chain of scopes: `{key: {value, source}}`.
    pub async fn resolve_chain(&self, chain: &[String]) -> Result<BTreeMap<String, Value>> {
        let mut out: BTreeMap<String, Value> = registry()
            .into_iter()
            .map(|d| {
                (
                    d.key.to_string(),
                    json!({"value": (d.default)(), "source": "default", "description": d.description}),
                )
            })
            .collect();
        for scope in chain {
            for (k, v) in self.scope_values(scope).await? {
                if let Some(slot) = out.get_mut(&k) {
                    slot["value"] = v;
                    slot["source"] = json!(scope);
                }
            }
        }
        Ok(out)
    }

    /// Resolved settings for a project, with per-actor settings taken from `actor`.
    pub async fn resolve_settings(
        &self,
        project: &str,
        actor: Option<&Principal>,
    ) -> Result<BTreeMap<String, Value>> {
        let mut out = self
            .resolve_chain(&self.project_scope_chain(project).await?)
            .await?;
        if let Some(a) = actor {
            let per_actor = self
                .resolve_chain(&["app".to_string(), format!("user:{}", human_of(a))])
                .await?;
            for d in registry().into_iter().filter(|d| d.per_actor) {
                if let Some(v) = per_actor.get(d.key) {
                    out.insert(d.key.to_string(), v.clone());
                }
            }
        }
        Ok(out)
    }

    /// One resolved setting value for a project.
    pub async fn setting(&self, project: &str, key: &str) -> Result<Value> {
        Ok(self
            .resolve_settings(project, None)
            .await?
            .get(key)
            .map(|v| v["value"].clone())
            .unwrap_or(Value::Null))
    }

    /// One setting resolved for a user (app → user), e.g. session lifetime.
    pub async fn setting_for_user(&self, user: &str, key: &str) -> Result<Value> {
        Ok(self
            .resolve_chain(&["app".to_string(), format!("user:{user}")])
            .await?
            .get(key)
            .map(|v| v["value"].clone())
            .unwrap_or(Value::Null))
    }

    /// May `by` change settings at `scope`?
    pub async fn can_manage_scope(&self, by: &Principal, scope: &str) -> Result<bool> {
        if !by.is_human() {
            return Ok(false);
        }
        Ok(match parse_scope(scope)? {
            ("app", _) => by.is_admin,
            ("user", u) => u == by.user,
            ("org", o) => self.org_role(o, &by.user).await? == Some("admin".into()),
            ("project", p) => self.project_permission(by, p).await? == Some(Permission::Admin),
            _ => false,
        })
    }

    /// May `by` read settings at `scope`?
    pub async fn can_view_scope(&self, by: &Principal, scope: &str) -> Result<bool> {
        Ok(match parse_scope(scope)? {
            ("app", _) => true,
            ("user", u) => u == by.user || u == human_of(by),
            ("org", o) => self.org_role(o, &human_of(by)).await?.is_some(),
            ("project", p) => self.project_permission(by, p).await?.is_some(),
            _ => false,
        })
    }

    /// Set (or clear, with `None`) a setting at a scope.
    pub async fn set_setting(
        &self,
        by: &Principal,
        scope: &str,
        key: &str,
        value: Option<Value>,
    ) -> Result<()> {
        parse_scope(scope)?;
        let def = definition(key)
            .ok_or_else(|| StoreError::Invalid(format!("unknown setting `{key}`")))?;
        if !self.can_manage_scope(by, scope).await? {
            return Err(StoreError::Invalid(format!(
                "you may not change settings at {scope}"
            )));
        }
        if key == "open_registration" && scope != "app" {
            return Err(StoreError::Invalid(
                "open_registration is an app-level setting".into(),
            ));
        }
        if def.per_actor && (scope.starts_with("org:") || scope.starts_with("project:")) {
            return Err(StoreError::Invalid(format!(
                "`{key}` is a per-user setting (set it at app or user scope)"
            )));
        }
        let k = setting_key(scope, key);
        match value {
            Some(v) => {
                validate_value(key, &v)?;
                self.db
                    .db_raw(
                        Method::POST,
                        &format!("/_api/document/{SETTINGS}?overwriteMode=replace"),
                        Some(&json!({"_key": k, "scope": scope, "key": key, "value": v,
                                     "set_by": by.id(), "set_at": now_iso()})),
                    )
                    .await?;
            }
            None => match self
                .db
                .db_raw(
                    Method::DELETE,
                    &format!("/_api/document/{SETTINGS}/{k}"),
                    None,
                )
                .await
            {
                Ok(_) | Err(StoreError::Arango { num: 1202, .. }) => {}
                Err(e) => return Err(e),
            },
        }
        Ok(())
    }

    /// A setting written by the system itself (the sysadmin agent's caps), not by a person.
    /// Human hard limits (`agent_caps_hard_limit`) are never written this way.
    pub async fn set_setting_system(
        &self,
        scope: &str,
        key: &str,
        value: Value,
        by: &str,
    ) -> Result<()> {
        parse_scope(scope)?;
        if !matches!(key, "agent_caps") {
            return Err(StoreError::Invalid(format!(
                "`{key}` is not a system-managed setting"
            )));
        }
        validate_value(key, &value)?;
        self.db
            .db_raw(
                Method::POST,
                &format!("/_api/document/{SETTINGS}?overwriteMode=replace"),
                Some(&json!({"_key": setting_key(scope, key), "scope": scope, "key": key, "value": value,
                             "set_by": by, "set_at": now_iso(), "system": true})),
            )
            .await?;
        Ok(())
    }

    /// Raw values stored at one scope.
    pub async fn scope_settings(&self, scope: &str) -> Result<BTreeMap<String, Value>> {
        parse_scope(scope)?;
        self.scope_values(scope).await
    }
}

/// The human user behind a principal (itself for a human).
pub fn human_of(p: &Principal) -> String {
    match p.on_behalf_of().as_deref().and_then(Owner::from_id) {
        Some(Owner::User(u)) => u,
        _ => p.user.clone(),
    }
}

fn validate_value(key: &str, v: &Value) -> Result<()> {
    let bad = |m: &str| Err(StoreError::Invalid(format!("{key}: {m}")));
    match key {
        "retry_limit"
        | "negotiation_rounds"
        | "lease_secs"
        | "test_levels_up"
        | "session_ttl_hours"
        | "token_max_lifetime_days" => match v.as_i64() {
            Some(n) if n >= 0 => Ok(()),
            _ => bad("must be a non-negative integer"),
        },
        "blind_review"
        | "parent_ready_requires_children"
        | "open_registration"
        | "kickoff"
        | "use_case_check" => {
            if v.is_boolean() {
                Ok(())
            } else {
                bad("must be true or false")
            }
        }
        "overlap_absorb_threshold" => match v.as_f64() {
            Some(x) if (0.0..=1.0).contains(&x) => Ok(()),
            _ => bad("must be between 0 and 1"),
        },
        "test_budget_pct" => match v.as_f64() {
            Some(x) if (0.0..=100.0).contains(&x) => Ok(()),
            _ => bad("must be a percentage"),
        },
        "human_engagement" => match v.as_str() {
            Some("contexts" | "containers" | "plans" | "none") => Ok(()),
            _ => bad("must be contexts, containers, plans or none"),
        },
        "agent_backend" => match v.as_str() {
            Some("claude_code" | "api" | "recorded") => Ok(()),
            _ => bad("must be claude_code, api or recorded"),
        },
        "agent_caps" => match v.as_object() {
            Some(m) if m.values().all(|x| x.as_u64().is_some()) => Ok(()),
            _ => bad("must map agent types to non-negative integers"),
        },
        "environments" => match v.as_array() {
            Some(a) if !a.is_empty() && a.iter().all(|e| e["name"].is_string()) => Ok(()),
            _ => bad("must be a non-empty list of {name, max_releases}"),
        },
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes() {
        assert!(parse_scope("app").is_ok());
        assert!(parse_scope("project:auth").is_ok());
        assert!(parse_scope("team:x").is_err());
        assert!(parse_scope("user:").is_err());
    }

    #[test]
    fn values_validated() {
        assert!(validate_value("retry_limit", &json!(1)).is_ok());
        assert!(validate_value("retry_limit", &json!(-1)).is_err());
        assert!(validate_value("overlap_absorb_threshold", &json!(1.5)).is_err());
        assert!(validate_value("human_engagement", &json!("all")).is_err());
    }

    #[test]
    fn registry_has_spec_defaults() {
        let d = |k: &str| (definition(k).unwrap().default)();
        assert_eq!(d("retry_limit"), json!(3));
        assert_eq!(d("test_cadence")["component_hours"], json!(2));
        assert_eq!(d("test_cadence")["project_hours"], json!(24));
        assert_eq!(d("blind_review"), json!(true));
    }
}
