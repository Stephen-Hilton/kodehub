//! Test container types (spec §5.4.1): per project, children of the project in the graph. Each
//! declares the layers it needs on the one core image; the test author agent keeps the set
//! viable. Bundles name the type they need (`rust`, `python`, `browser`).

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
            d["created_at"] = json!(now_iso());
            d["source"] = json!("default");
            self.db
                .insert_ignore("test_container_types", &d, None)
                .await?;
        }
        Ok(())
    }

    pub async fn test_container_types(&self) -> Result<Vec<Value>> {
        self.ensure_test_container_types().await?;
        self.aql(
            "FOR t IN @@test_container_types SORT t._key RETURN t",
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
        doc["_key"] = json!(crate::slug(name));
        doc["updated_at"] = json!(now_iso());
        doc["updated_by"] = json!(by);
        self.db.upsert("test_container_types", &doc, None).await?;
        self.db
            .get("test_container_types", &crate::slug(name), None)
            .await
    }
}
