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
        let from = dump["project"].as_str().unwrap_or_default().to_string();
        // documents first, then edges (their endpoints must exist)
        for c in DOCUMENT_COLLECTIONS.iter().chain(EDGE_COLLECTIONS) {
            for d in dump["collections"][c]
                .as_array()
                .cloned()
                .unwrap_or_default()
            {
                let mut d = rekey(&d, &from, &id);
                if let Some(m) = d.as_object_mut() {
                    m.remove("project");
                }
                // environments are recreated by bootstrap; replace keeps the backed-up state
                ps.db.upsert(c, &d, None).await?;
            }
        }
        Ok(json!({"project": id, "restored_from": dump["project"], "backup_at": dump["at"]}))
    }
}

/// Fields that hold free text or content, never ids: left untouched by `rekey` (code bodies
/// must keep their content hashes).
const TEXT_FIELDS: &[&str] = &[
    "body",
    "signature",
    "text",
    "statement",
    "intent",
    "description",
    "output",
    "summary",
    "message",
    "notes",
    "note",
    "raw_transcript",
    "transcripts",
    "stdout",
    "stderr",
    "finding",
    "attributes",
    "definition",
];

/// Move a document from project `from` to project `to`: every string (and object key) that is
/// exactly a key (`from.x`) or an id (`coll/from.x`) of the source project is re-qualified.
pub fn rekey(v: &Value, from: &str, to: &str) -> Value {
    fn one(s: &str, from: &str, to: &str) -> Option<String> {
        // the project's own registry node moves with it
        for reg in ["projects", crate::schema::PROJECTS] {
            if s.strip_prefix(reg).and_then(|r| r.strip_prefix('/')) == Some(from) {
                return Some(format!("{reg}/{to}"));
            }
        }
        let (prefix, key) = match s.split_once('/') {
            Some((c, k)) if crate::arango::is_project_collection(c) => (format!("{c}/"), k),
            Some(_) => return None,
            None => (String::new(), s),
        };
        let rest = key
            .strip_prefix(from)?
            .strip_prefix(crate::arango::KEY_SEP)?;
        crate::arango::valid_key(rest)
            .then(|| format!("{prefix}{}", crate::arango::qualify(to, rest)))
    }
    match v {
        Value::String(s) => Value::String(one(s, from, to).unwrap_or_else(|| s.clone())),
        Value::Array(a) => Value::Array(a.iter().map(|x| rekey(x, from, to)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| {
                    let k2 = one(k, from, to).unwrap_or_else(|| k.clone());
                    let x2 = if TEXT_FIELDS.contains(&k.as_str()) {
                        x.clone()
                    } else {
                        rekey(x, from, to)
                    };
                    (k2, x2)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rekey_moves_ids_but_not_content() {
        let d = json!({
            "_key": "shop.rust-x", "_from": "code/shop.a", "_to": "projects/shop",
            "list": ["shop.r1", "shop", "shopping.x"], "map": {"shop.k": 1},
            "body": "use shop.x;", "url": "http://shop.example"
        });
        let r = rekey(&d, "shop", "copy");
        assert_eq!(r["_key"], "copy.rust-x");
        assert_eq!(r["_from"], "code/copy.a");
        assert_eq!(r["_to"], "projects/copy");
        assert_eq!(r["list"], json!(["copy.r1", "shop", "shopping.x"]));
        assert_eq!(r["map"], json!({"copy.k": 1}));
        assert_eq!(r["body"], "use shop.x;");
        assert_eq!(r["url"], "http://shop.example");
    }
}
