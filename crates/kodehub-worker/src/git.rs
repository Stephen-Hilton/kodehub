//! Git import and export containers (spec §13). Filled in by S9.

use anyhow::Result;
use serde_json::Value;

pub async fn import(_input: &Value) -> Result<Value> {
    anyhow::bail!("git import is not available yet")
}

pub async fn export(_input: &Value) -> Result<Value> {
    anyhow::bail!("git export is not available yet")
}
