//! Audit trail (spec §13A.4): every API action is logged with the acting identity, the human it
//! acted for, the project, the action and the time. Credentials never appear here.

use kodehub_core::model::now_iso;
use serde_json::{json, Value};

use crate::identity::{AuthVia, Principal, UserKind};
use crate::{Result, Store};

pub const AUDIT: &str = "kodehub_audit";

/// `human` | `external_agent` | `containerized_agent` — the three kinds admins filter by.
pub fn actor_class(p: &Principal) -> &'static str {
    match (p.kind, p.via) {
        (UserKind::Human, _) => "human",
        (UserKind::Agent, AuthVia::Internal) => "containerized_agent",
        (UserKind::Agent, _) => "external_agent",
    }
}

/// Strip anything that could carry a secret from a recorded detail value.
fn scrub(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| {
                    let lk = k.to_ascii_lowercase();
                    if [
                        "secret",
                        "password",
                        "token",
                        "key",
                        "authorization",
                        "credential",
                    ]
                    .iter()
                    .any(|s| lk.contains(s))
                    {
                        (k.clone(), json!("[redacted]"))
                    } else {
                        (k.clone(), scrub(x))
                    }
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(scrub).collect()),
        Value::String(s)
            if s.starts_with("khs_")
                || s.starts_with("khp_")
                || s.starts_with("khi_")
                || s.starts_with("sk-ant-") =>
        {
            json!("[redacted]")
        }
        other => other.clone(),
    }
}

#[derive(Debug, Default, Clone)]
pub struct AuditFilter {
    pub project: Option<String>,
    pub user: Option<String>,
    pub actor_class: Option<String>,
    pub limit: usize,
}

impl Store {
    pub async fn audit(
        &self,
        by: Option<&Principal>,
        project: Option<&str>,
        action: &str,
        status: u16,
        detail: Value,
    ) -> Result<()> {
        let doc = json!({
            "at": now_iso(),
            "actor": by.map(|p| p.id()),
            "actor_class": by.map(actor_class),
            "on_behalf_of": by.and_then(|p| p.on_behalf_of()),
            "token": by.and_then(|p| p.token_id.clone()),
            "project": project,
            "action": action,
            "status": status,
            "detail": scrub(&detail),
        });
        self.g_insert(AUDIT, &doc).await.map(|_| ())
    }

    pub async fn list_audit(&self, f: &AuditFilter) -> Result<Vec<Value>> {
        self.g_aql(
            "FOR a IN @@a
               FILTER @project == null OR a.project == @project
               FILTER @user == null OR a.actor == @user OR a.on_behalf_of == @user
               FILTER @class == null OR a.actor_class == @class
               SORT a.at DESC LIMIT @limit RETURN UNSET(a, '_rev')",
            json!({
                "@a": AUDIT, "project": f.project, "user": f.user, "class": f.actor_class,
                "limit": if f.limit == 0 { 200 } else { f.limit.min(2000) },
            }),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_secrets() {
        let v = scrub(&json!({"password": "hunter22", "name": "x",
                              "nested": {"api_key": "sk-ant-api03-x"}, "arg": "khp_abc"}));
        let s = v.to_string();
        assert!(!s.contains("hunter22") && !s.contains("sk-ant") && !s.contains("khp_abc"));
        assert!(s.contains("\"name\":\"x\""));
    }
}
