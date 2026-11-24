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
    contexts: &[String],
) -> Result<Value, ApiError> {
    let store = s.store.clone();
    let by = by.id();
    let philosophy = philosophy.map(str::to_string);
    let contexts = contexts.to_vec();
    kodehub_store::with_project(project.to_string(), async move {
        let mut made = Vec::new();
        if let Some(text) = philosophy.filter(|t| !t.trim().is_empty()) {
            store.set_philosophy(&text, &by).await?;
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
                    json!({"_key": kodehub_store::slug(name), "name": name, "description": desc,
                           "lifecycle": "planned"}),
                )
                .await?;
            made.push(doc["_id"].clone());
        }
        Ok::<_, ApiError>(json!({"contexts": made}))
    })
    .await
}
