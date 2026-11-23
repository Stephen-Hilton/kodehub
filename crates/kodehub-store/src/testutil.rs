//! Helpers for integration tests: a throwaway database per test.

use crate::{ArangoConfig, Result, Store};

/// A store bound to a fresh, uniquely named database. Call `drop_db` at the end of the test.
pub async fn temp_store(prefix: &str) -> Result<Store> {
    let mut cfg = ArangoConfig::from_env();
    cfg.database = format!(
        "lt_{prefix}_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..10]
    );
    Store::open(cfg).await
}

impl Store {
    pub async fn drop_db(&self) -> Result<()> {
        self.db.drop_database().await
    }
}

/// Path to the repo's seed dataset.
pub fn seed_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/auth-poc")
        .canonicalize()
        .expect("examples/auth-poc exists")
}
