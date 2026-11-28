//! Container images (spec §5.4.1, docs/ontology.md `container_images`): per project, owned by
//! the project. Test images declare the layers they need on the one core image; the test author
//! agent keeps the set viable. Bundles name the image they need (`rust`, `python`, `browser`).
//! Deploy images (`purpose: deploy`) are run by deployers (`deploy.rs`).

use kodehub_core::model::now_iso;
use serde_json::{json, Value};

use crate::{Result, Store, StoreError};

pub const LAYERS: &[&str] = &["core", "rust", "python", "browser"];

pub fn defaults() -> Vec<Value> {
    vec![
        json!({"_key": "rust", "layers": ["core", "rust"], "image": "kodehub/test-rust:dev",
               "requires": ["rust"], "description": "Rust test binaries (built once, run from the bundle)"}),
        json!({"_key": "python", "layers": ["core", "python"], "image": "kodehub/test-python:dev",
               "requires": ["python>=3.12"], "description": "Python package + vendored dependencies + pytest"}),
        json!({"_key": "browser", "layers": ["core", "python", "browser"], "image": "kodehub/test-browser:dev",
               "requires": ["python>=3.12", "chromium"], "description": "Headless browser for front-end use case tests"}),
    ]
}

impl Store {
    pub async fn ensure_test_container_types(&self) -> Result<()> {
        for d in defaults() {
            let mut d = d;
            let name = d["_key"].as_str().unwrap_or_default().to_string();
            d["_key"] = json!(self.db.pkey(&name)?);
            d["name"] = json!(name);
            d["purpose"] = json!("test");
            d["created_at"] = json!(now_iso());
            d["source"] = json!("default");
            let id = format!(
                "container_images/{}",
                d["_key"].as_str().unwrap_or_default()
            );
            if self.get_node(&id).await.is_err() {
                self.db.insert_ignore("container_images", &d, None).await?;
                self.own(&id).await?;
            }
        }
        Ok(())
    }

    pub async fn test_container_types(&self) -> Result<Vec<Value>> {
        self.ensure_test_container_types().await?;
        self.aql(
            "FOR t IN @@container_images FILTER t.purpose != 'deploy' SORT t.name RETURN t",
            json!({}),
        )
        .await
    }

    /// Create or change a type (test author agent / project admins).
    pub async fn put_test_container_type(
        &self,
        name: &str,
        mut doc: Value,
        by: &str,
    ) -> Result<Value> {
        let layers: Vec<String> = doc["layers"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if layers.first().map(String::as_str) != Some("core") {
            return Err(StoreError::Invalid(
                "every test container type starts from the core layer".into(),
            ));
        }
        if let Some(bad) = layers.iter().find(|l| !LAYERS.contains(&l.as_str())) {
            return Err(StoreError::Invalid(format!("unknown layer `{bad}`")));
        }
        doc["_key"] = json!(self.db.pkey(&crate::slug(name))?);
        doc["name"] = json!(crate::slug(name));
        doc["purpose"] = json!("test");
        doc["updated_at"] = json!(now_iso());
        doc["updated_by"] = json!(by);
        self.db.upsert("container_images", &doc, None).await?;
        self.db
            .get("container_images", &crate::slug(name), None)
            .await
    }
}
