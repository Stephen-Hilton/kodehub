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
