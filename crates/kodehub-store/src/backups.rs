//! Per-project database backups (spec §3.4, §13): a JSON dump of one project's graph, written to
//! the storage service (whose hybrid mode ships it off-site with the project's other objects),
//! and restored alone into a new project.

use kodehub_core::model::{now_iso, DOCUMENT_COLLECTIONS, EDGE_COLLECTIONS};
use serde_json::{json, Value};

use crate::{Result, Store, StoreError};

impl Store {
    /// Dump every collection of a project (logical ids).
    pub async fn export_project(&self, project: &str) -> Result<Value> {
        let ps = self.for_project(project);
        let mut cols = serde_json::Map::new();
        for c in DOCUMENT_COLLECTIONS.iter().chain(EDGE_COLLECTIONS) {
            let docs = ps
                .aql(
                    "FOR d IN @@c RETURN UNSET(d, '_id', '_rev')",
                    json!({"@c": c}),
                )
                .await?;
            if !docs.is_empty() {
                cols.insert(c.to_string(), Value::Array(docs));
            }
        }
        Ok(
            json!({"format": "kodehub-project-backup/1", "project": project, "at": now_iso(),
                  "registry": self.g_get(crate::schema::PROJECTS, project).await?, "collections": cols}),
        )
    }

    /// Restore a dump into a new project owned by `owner` (the original is untouched).
    pub async fn restore_project(
        &self,
        dump: &Value,
        name: &str,
        owner: &crate::Owner,
    ) -> Result<Value> {
        if dump["format"] != "kodehub-project-backup/1" {
            return Err(StoreError::Invalid("not a Kodehub project backup".into()));
        }
        let created = self
            .create_project(name, dump["registry"]["description"].as_str().unwrap_or(""))
            .await?;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        self.set_project_owner(&id, owner).await?;
        let ps = self.for_project(&id);
        // documents first, then edges (their endpoints must exist)
        for c in DOCUMENT_COLLECTIONS.iter().chain(EDGE_COLLECTIONS) {
            for d in dump["collections"][c]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                // environments are recreated by bootstrap; replace keeps the backed-up state
                ps.db.upsert(c, &d, None).await?;
            }
        }
        Ok(json!({"project": id, "restored_from": dump["project"], "backup_at": dump["at"]}))
    }
}
