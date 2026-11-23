//! Projects: many projects in one ArangoDB database (decision D-projects).
//!
//! The registry (`kodehub_projects`) lists projects; `kodehub_meta/active` remembers the active
//! one across restarts. Each project owns its own collection set (see `arango.rs`). Switching
//! is server-wide: every clone of the store follows the active project.

use std::path::Path;

use kodehub_core::model::{now_iso, DOCUMENT_COLLECTIONS, EDGE_COLLECTIONS};
use reqwest::Method;
use serde_json::{json, Value};

use crate::arango::{valid_project_id, Arango, GRAPH_NAME};
use crate::schema::{self, META, PROJECTS};
use crate::{Result, Store, StoreError};

/// Name given to the project created when the database has none.
pub const FIRST_PROJECT_NAME: &str = "My first project";

/// Derive a project id from a display name: `[a-z][a-z0-9_]*`, max 32 chars.
pub fn project_id_from_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let mut id: String = out.trim_matches('_').chars().take(28).collect();
    id = id.trim_end_matches('_').to_string();
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        id = format!("p_{id}").trim_end_matches('_').to_string();
    }
    if id == "p" || id.is_empty() {
        id = "project".into();
    }
    id
}

fn validate_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 80 {
        return Err(StoreError::Invalid(
            "project name must be 1–80 characters".into(),
        ));
    }
    Ok(n.to_string())
}

impl Arango {
    /// An independent handle bound to `project` (does not affect the shared active project).
    pub fn scoped(&self, project: &str) -> Arango {
        let a = Arango::new(self.cfg.clone());
        a.set_project(Some(project.to_string()));
        a
    }

    async fn collection_exists(&self, physical: &str) -> Result<bool> {
        match self
            .db_raw(Method::GET, &format!("/_api/collection/{physical}"), None)
            .await
        {
            Ok(_) => Ok(true),
            Err(StoreError::Arango { num: 1203, .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

impl Store {
    /// The active project id.
    pub fn project_id(&self) -> Option<String> {
        self.db.project()
    }

    /// Select the active project at startup: the remembered one, else the most recently
    /// opened, else a freshly created first project. Migrates a pre-projects database first.
    pub(crate) async fn select_initial_project(&self, backup_dir: Option<&Path>) -> Result<()> {
        schema::bootstrap_registry(&self.db).await?;
        self.migrate_legacy(backup_dir).await?;
        let remembered = self
            .meta("active")
            .await?
            .and_then(|m| m.get("project").and_then(Value::as_str).map(str::to_string));
        let projects = self.list_projects_raw().await?;
        let pick = remembered
            .filter(|r| projects.iter().any(|p| p["_key"] == *r))
            .or_else(|| {
                projects
                    .first()
                    .and_then(|p| p["_key"].as_str())
                    .map(str::to_string)
            });
        let id = match pick {
            Some(id) => id,
            None => self.register_project(FIRST_PROJECT_NAME, "").await?["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        };
        self.activate(&id).await
    }

    async fn meta(&self, key: &str) -> Result<Option<Value>> {
        match self
            .db
            .db_raw(Method::GET, &format!("/_api/document/{META}/{key}"), None)
            .await
        {
            Ok(v) => Ok(Some(v)),
            Err(StoreError::Arango { num: 1202, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn set_meta(&self, key: &str, mut doc: Value) -> Result<()> {
        doc["_key"] = json!(key);
        self.db
            .db_raw(
                Method::POST,
                &format!("/_api/document/{META}?overwriteMode=replace"),
                Some(&doc),
            )
            .await
            .map(|_| ())
    }

    async fn list_projects_raw(&self) -> Result<Vec<Value>> {
        self.db
            .aql_raw(
                "FOR p IN @@p SORT p.last_opened_at DESC, p.created_at DESC RETURN p",
                json!({"@p": PROJECTS}),
            )
            .await
    }

    /// Every project, most recently opened first, with `active` and a few counts.
    pub async fn list_projects(&self) -> Result<Vec<Value>> {
        let active = self.project_id();
        let mut out = Vec::new();
        for p in self.list_projects_raw().await? {
            let id = p["_key"].as_str().unwrap_or_default().to_string();
            let counts = self
                .db
                .aql_raw(
                    "RETURN {requirements: LENGTH(@@r), symbols: LENGTH(@@s), changesets: LENGTH(@@c)}",
                    json!({
                        "@r": Arango::phys_in(&id, "requirements"),
                        "@s": Arango::phys_in(&id, "symbols"),
                        "@c": Arango::phys_in(&id, "changesets"),
                    }),
                )
                .await
                .ok()
                .and_then(|r| r.into_iter().next())
                .unwrap_or(Value::Null);
            out.push(json!({
                "id": id,
                "name": p["name"],
                "description": p["description"],
                "created_at": p["created_at"],
                "last_opened_at": p["last_opened_at"],
                "active": active.as_deref() == Some(id.as_str()),
                "counts": counts,
            }));
        }
        Ok(out)
    }

    /// The active project's registry entry.
    pub async fn active_project(&self) -> Result<Value> {
        let id = self
            .project_id()
            .ok_or_else(|| StoreError::Invalid("no project selected".into()))?;
        self.list_projects()
            .await?
            .into_iter()
            .find(|p| p["id"] == id)
            .ok_or_else(|| StoreError::NotFound(format!("project {id}")))
    }

    async fn project_doc(&self, id: &str) -> Result<Value> {
        if !valid_project_id(id) {
            return Err(StoreError::Invalid(format!("invalid project id `{id}`")));
        }
        match self
            .db
            .db_raw(
                Method::GET,
                &format!("/_api/document/{PROJECTS}/{id}"),
                None,
            )
            .await
        {
            Ok(v) => Ok(v),
            Err(StoreError::Arango { num: 1202, .. }) => {
                Err(StoreError::NotFound(format!("project {id}")))
            }
            Err(e) => Err(e),
        }
    }

    /// Register a project and create its schema (does not switch to it).
    async fn register_project(&self, name: &str, description: &str) -> Result<Value> {
        let name = validate_name(name)?;
        let base = project_id_from_name(&name);
        let mut id = base.clone();
        let mut n = 2;
        loop {
            let doc = json!({
                "_key": id, "name": name, "description": description.trim(),
                "created_at": now_iso(), "last_opened_at": null,
            });
            match self
                .db
                .db_raw(
                    Method::POST,
                    &format!("/_api/document/{PROJECTS}"),
                    Some(&doc),
                )
                .await
            {
                Ok(_) => break,
                Err(StoreError::Arango { num: 1210, .. }) => {
                    let suffix = format!("_{n}");
                    let stem: String = base.chars().take(32 - suffix.len()).collect();
                    id = format!("{}{suffix}", stem.trim_end_matches('_'));
                    n += 1;
                }
                Err(e) => return Err(e),
            }
        }
        schema::bootstrap(&self.db.scoped(&id)).await?;
        Ok(json!({"id": id, "name": name, "description": description.trim()}))
    }

    /// Create a project (empty graph, four environments). Does not switch to it.
    pub async fn create_project(&self, name: &str, description: &str) -> Result<Value> {
        let created = self.register_project(name, description).await?;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        self.list_projects()
            .await?
            .into_iter()
            .find(|p| p["id"] == id)
            .ok_or_else(|| StoreError::NotFound(format!("project {id}")))
    }

    async fn activate(&self, id: &str) -> Result<()> {
        self.project_doc(id).await?;
        // idempotent: heals a project whose schema is incomplete
        schema::bootstrap(&self.db.scoped(id)).await?;
        self.db.set_project(Some(id.to_string()));
        self.set_meta("active", json!({"project": id})).await?;
        self.db
            .db_raw(
                Method::PATCH,
                &format!("/_api/document/{PROJECTS}/{id}"),
                Some(&json!({"last_opened_at": now_iso()})),
            )
            .await?;
        Ok(())
    }

    /// Switch the server-wide active project. Waits for in-flight merges/deployments.
    pub async fn switch_project(&self, id: &str) -> Result<Value> {
        let _w = self.write_lock.lock().await;
        self.activate(id).await?;
        self.active_project().await
    }

    /// Rename / re-describe a project.
    pub async fn update_project(
        &self,
        id: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<Value> {
        self.project_doc(id).await?;
        let mut patch = json!({});
        if let Some(n) = name {
            patch["name"] = json!(validate_name(n)?);
        }
        if let Some(d) = description {
            patch["description"] = json!(d.trim());
        }
        self.db
            .db_raw(
                Method::PATCH,
                &format!("/_api/document/{PROJECTS}/{id}"),
                Some(&patch),
            )
            .await?;
        self.list_projects()
            .await?
            .into_iter()
            .find(|p| p["id"] == id)
            .ok_or_else(|| StoreError::NotFound(format!("project {id}")))
    }

    /// Delete a project and all of its data. The active project cannot be deleted.
    pub async fn delete_project(&self, id: &str) -> Result<()> {
        let _w = self.write_lock.lock().await;
        self.project_doc(id).await?;
        if self.project_id().as_deref() == Some(id) {
            return Err(StoreError::Conflict(
                "the open project cannot be deleted — switch to another project first".into(),
            ));
        }
        schema::drop_project_schema(&self.db, id).await?;
        self.db
            .db_raw(
                Method::DELETE,
                &format!("/_api/document/{PROJECTS}/{id}"),
                None,
            )
            .await?;
        Ok(())
    }

    /// Move a pre-projects database (unprefixed collections) into a project of its own.
    /// The data is copied first; the old collections are dropped only when `backup_dir` is
    /// given, after a JSON backup of them has been written there.
    async fn migrate_legacy(&self, backup_dir: Option<&Path>) -> Result<()> {
        if !self.db.collection_exists("symbols").await? {
            return Ok(());
        }
        let legacy: Vec<&str> = {
            let mut v = Vec::new();
            for c in DOCUMENT_COLLECTIONS.iter().chain(EDGE_COLLECTIONS) {
                if self.db.collection_exists(c).await? {
                    v.push(*c);
                }
            }
            v
        };
        if self.meta("legacy").await?.is_none() {
            let title = self
                .db
                .aql_raw(
                    "FOR m IN missions SORT m._key LIMIT 1 RETURN m.title",
                    json!({}),
                )
                .await
                .ok()
                .and_then(|r| r.into_iter().next())
                .and_then(|v| v.as_str().map(str::to_string))
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "Kodehub".into());
            let created = self.register_project(&title, "").await?;
            let id = created["id"].as_str().unwrap_or_default().to_string();
            let target = self.db.scoped(&id);
            for c in &legacy {
                let docs = self
                    .db
                    .aql_raw(
                        "FOR d IN @@c RETURN UNSET(d, '_id', '_rev')",
                        json!({"@c": c}),
                    )
                    .await?;
                for chunk in docs.chunks(500) {
                    target
                        .db(
                            Method::POST,
                            &format!("/_api/document/{}?overwriteMode=replace", target.phys(c)),
                            Some(&Value::Array(chunk.to_vec())),
                        )
                        .await?;
                }
                let n = target
                    .aql_raw("RETURN LENGTH(@@c)", json!({"@c": target.phys(c)}))
                    .await?;
                if n.first().and_then(Value::as_u64) < Some(docs.len() as u64) {
                    return Err(StoreError::Invariant(format!(
                        "legacy migration of `{c}` copied {n:?} of {} documents",
                        docs.len()
                    )));
                }
            }
            self.set_meta(
                "legacy",
                json!({"migrated_to": id, "at": now_iso(), "dropped": false}),
            )
            .await?;
            self.set_meta("active", json!({"project": id})).await?;
            tracing::info!("migrated the pre-projects database into project `{id}`");
        }
        let Some(dir) = backup_dir else {
            return Ok(());
        };
        std::fs::create_dir_all(dir)
            .map_err(|e| StoreError::Invalid(format!("backup dir: {e}")))?;
        let mut backup = serde_json::Map::new();
        for c in &legacy {
            let docs = self
                .db
                .aql_raw("FOR d IN @@c RETURN d", json!({"@c": c}))
                .await?;
            backup.insert(c.to_string(), Value::Array(docs));
        }
        let file = dir.join(format!(
            "pre-projects-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ));
        std::fs::write(
            &file,
            serde_json::to_vec(&Value::Object(backup)).unwrap_or_default(),
        )
        .map_err(|e| StoreError::Invalid(format!("writing backup: {e}")))?;
        match self
            .db
            .db_raw(
                Method::DELETE,
                &format!("/_api/gharial/{GRAPH_NAME}?dropCollections=false"),
                None,
            )
            .await
        {
            Ok(_) | Err(StoreError::Arango { num: 1924, .. }) => {}
            Err(e) => return Err(e),
        }
        for c in &legacy {
            self.db
                .db_raw(Method::DELETE, &format!("/_api/collection/{c}"), None)
                .await?;
        }
        let mut m = self.meta("legacy").await?.unwrap_or(json!({}));
        m["dropped"] = json!(true);
        m["backup"] = json!(file.display().to_string());
        self.set_meta("legacy", m).await?;
        tracing::info!(
            "dropped the pre-projects collections (backup: {})",
            file.display()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_from_names() {
        assert_eq!(
            project_id_from_name("Secure authentication"),
            "secure_authentication"
        );
        assert_eq!(project_id_from_name("  My  App!! "), "my_app");
        assert_eq!(project_id_from_name("42 things"), "p_42_things");
        assert_eq!(project_id_from_name("???"), "project");
        assert!(valid_project_id(&project_id_from_name(
            "A very very long project name that keeps going"
        )));
        for n in [
            "Secure authentication",
            "42 things",
            "???",
            "x__y",
            "Ünïcode",
        ] {
            assert!(valid_project_id(&project_id_from_name(n)), "{n}");
        }
    }

    #[test]
    fn id_rules() {
        assert!(valid_project_id("auth"));
        assert!(!valid_project_id("a__b"));
        assert!(!valid_project_id("1abc"));
        assert!(!valid_project_id("abc_"));
        assert!(!valid_project_id("Abc"));
        assert!(!valid_project_id(""));
    }
}
