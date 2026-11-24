//! Git import: staged code units and the import agent's C4 mapping (spec §13.1). Filled in by S9.

use serde_json::Value;

use crate::{Result, Store, StoreError};

pub async fn apply_mapping(_s: &Store, _mapping: &Value, _by: &str) -> Result<Value> {
    Err(StoreError::Invalid("no import is staged".into()))
}
