//! New-project initialization (spec §6.1): the philosophy document and starting contexts.

use kodehub_store::Principal;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::AppState;

/// Common starting contexts offered by the create-project flow.
pub const COMMON_CONTEXTS: &[(&str, &str)] = &[
    ("Web System", "User-facing web applications and their APIs."),
    ("Data Platform", "Storage, pipelines and analytics."),
    (
        "Authentication & Authorization",
        "Identity, sign-in and access control.",
    ),
    ("Integrations", "Connections to external services."),
    ("Operations", "Deployment, monitoring and internal tooling."),
];

/// The built-in master template: common contexts with best-practice requirements placed at the
/// lowest level where they apply (spec §6.1, §7.2).
pub fn master_template() -> serde_json::Value {
    json!({
        "contexts": ["Authentication & Authorization", "Data Platform", "Web System", "Operations"],
        "requirements": [
            {"_key": "tpl-authn-every-call", "context": "Authentication & Authorization", "kind": "technical",
             "title": "Every call is authenticated", "statement": "Every API call is authenticated and scoped to the caller's permissions."},
            {"_key": "tpl-secrets-encrypted", "context": "Data Platform", "kind": "technical",
             "title": "Secrets are encrypted at rest", "statement": "Credentials and secrets are stored encrypted and never logged."},
            {"_key": "tpl-backups", "context": "Data Platform", "kind": "business",
             "title": "Data survives the loss of a machine", "statement": "Every project's data is backed up off-site and can be restored alone."},
            {"_key": "tpl-web-accessible", "context": "Web System", "kind": "business",
             "title": "Accessible interface", "statement": "Every screen is usable with a keyboard and a screen reader."},
            {"_key": "tpl-ops-health", "context": "Operations", "kind": "technical",
             "title": "Health is observable", "statement": "Every deployable unit exposes a health check."}
        ]
    })
}

/// Copy contexts, requirements and the requirement edges between them from another project
/// (a template from a project the caller can read). Code is never copied.
pub async fn copy_template(s: &AppState, from: &str, to: &str) -> Result<Value, ApiError> {
    let src = s.store.for_project(from);
    let dst = s.store.for_project(to);
    let mut copied = json!({"contexts": 0, "requirements": 0});
    for coll in ["contexts", "requirement_groups", "requirements"] {
        for mut d in src
            .aql(
                "FOR d IN @@c RETURN UNSET(d, '_id', '_rev')",
                json!({"@c": coll}),
            )
            .await?
        {
            if coll == "requirements" {
                d["revision"] = json!(1);
                d["covered_revision"] = json!(0);
                d["at_risk"] = json!(false);
            }
            if coll == "contexts" {
                d["lifecycle"] = json!("planned");
            }
            dst.db.upsert(coll, &d, None).await?;
            if let Some(n) = copied.get_mut(coll).and_then(|v| v.as_u64()) {
                copied[coll] = json!(n + 1);
            }
        }
    }
    for coll in ["satisfies", "includes", "contains"] {
        let rows = src
            .aql(
                "FOR e IN @@c FILTER !STARTS_WITH(e._from, CONCAT(@pfx, 'symbols/')) AND !STARTS_WITH(e._to, CONCAT(@pfx, 'symbols/'))
                   AND !STARTS_WITH(e._to, CONCAT(@pfx, 'containers/')) AND !STARTS_WITH(e._from, CONCAT(@pfx, 'containers/'))
                   RETURN {_from: e._from, _to: e._to}",
                json!({"@c": coll}),
            )
            .await?;
        for e in rows {
            let (f, t) = (
                e["_from"].as_str().unwrap_or(""),
                e["_to"].as_str().unwrap_or(""),
            );
            if dst.get_node(f).await.is_ok() && dst.get_node(t).await.is_ok() {
                let k = kodehub_store::edge_key(coll, f, t);
                dst.db
                    .insert_ignore(coll, &json!({"_key": k, "_from": f, "_to": t}), None)
                    .await?;
            }
        }
    }
    Ok(copied)
}

/// Create the philosophy document and the chosen contexts inside the new project.
pub async fn initialize(
    s: &AppState,
    by: &Principal,
    project: &str,
    philosophy: Option<&str>,
    raw_transcript: Option<&str>,
    contexts: &[String],
) -> Result<Value, ApiError> {
    let store = s.store.clone();
    let by = by.id();
    let raw = raw_transcript.map(str::to_string);
    let philosophy = philosophy.map(str::to_string).or_else(|| raw.clone());
    let contexts = contexts.to_vec();
    kodehub_store::with_project(project.to_string(), async move {
        let mut made = Vec::new();
        if let Some(text) = philosophy.filter(|t| !t.trim().is_empty()) {
            store
                .set_philosophy_with(&text, &by, None, raw.as_deref())
                .await?;
        }
        for name in contexts {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let desc = COMMON_CONTEXTS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| *d)
                .unwrap_or("");
            let doc = store
                .insert_node(
                    "contexts",
                    json!({"_key": kodehub_store::slug(name), "title": name, "name": name,
                           "description": desc, "lifecycle": "planned"}),
                )
                .await?;
            let id = doc["_id"].as_str().unwrap_or_default().to_string();
            // contexts hang off the project root (the philosophy node)
            if store.get_philosophy().await?.is_some() {
                let k = kodehub_store::edge_key("contains", kodehub_store::c4::PHILOSOPHY_ID, &id);
                store
                    .db
                    .insert_ignore(
                        "contains",
                        &json!({"_key": k, "_from": kodehub_store::c4::PHILOSOPHY_ID, "_to": id}),
                        None,
                    )
                    .await?;
            }
            made.push(json!(id));
        }
        Ok::<_, ApiError>(json!({"contexts": made}))
    })
    .await
}
