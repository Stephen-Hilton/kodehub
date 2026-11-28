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
    // keys and ids move from the source project to the target project (D-055)
    let rekey = |k: &str| kodehub_store::arango::qualify(to, kodehub_store::arango::local_key(k));
    let reid = |id: &str| match id.split_once('/') {
        Some((c, k)) => format!("{c}/{}", rekey(k)),
        None => id.to_string(),
    };
    // contexts are placed under the target's project node (into its current project version)
    let root = dst.project_node()?;
    for mut d in src
        .aql(
            "FOR d IN @@contexts RETURN UNSET(d, '_id', '_rev', 'project')",
            json!({}),
        )
        .await?
    {
        d["_key"] = json!(rekey(d["_key"].as_str().unwrap_or_default()));
        d["lifecycle"] = json!("planned");
        let id = format!("contexts/{}", d["_key"].as_str().unwrap_or_default());
        if dst.get_node(&id).await.is_err() {
            dst.insert_node("contexts", d).await?;
            dst.insert_edge("contains", &root, &id, json!({})).await?;
            copied["contexts"] = json!(copied["contexts"].as_u64().unwrap_or(0) + 1);
        }
    }
    // accepted requirements start over at revision 1 (with their first version)
    for d in src
        .aql(
            "FOR d IN @@requirements FILTER d.state == null OR d.state == 'accepted'
               RETURN UNSET(d, '_id', '_rev', 'project', 'revision', 'covered_revision', 'at_risk',
                            'created_at', 'updated_at')",
            json!({}),
        )
        .await?
    {
        let mut d = d;
        d["_key"] = json!(rekey(d["_key"].as_str().unwrap_or_default()));
        if dst
            .get_node(&format!(
                "requirements/{}",
                d["_key"].as_str().unwrap_or_default()
            ))
            .await
            .is_err()
        {
            dst.create_requirement(d).await?;
            copied["requirements"] = json!(copied["requirements"].as_u64().unwrap_or(0) + 1);
        }
    }
    // what each requirement applies to: the project or a copied context
    let src_root = src.project_node()?;
    let rows = src
        .aql(
            "FOR e IN @@applies_to FILTER STARTS_WITH(e._from, 'requirements/')
               AND (STARTS_WITH(e._to, 'contexts/') OR STARTS_WITH(e._to, 'projects/'))
               RETURN {_from: e._from, _to: e._to}",
            json!({}),
        )
        .await?;
    for e in rows {
        let f = reid(e["_from"].as_str().unwrap_or(""));
        let t = e["_to"].as_str().unwrap_or("");
        let t = if t == src_root { root.clone() } else { reid(t) };
        if dst.get_node(&f).await.is_ok() && dst.get_node(&t).await.is_ok() {
            dst.insert_edge("applies_to", &f, &t, json!({})).await?;
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
            // contexts hang off the project node; placing puts them in the current version
            let root = store.project_node()?;
            store.insert_edge("contains", &root, &id, json!({})).await?;
            made.push(json!(id));
        }
        Ok::<_, ApiError>(json!({"contexts": made}))
    })
    .await
}
