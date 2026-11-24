//! Token metering and spend per project (spec §12.2).

use kodehub_core::model::now_iso;
use serde_json::{json, Value};

use crate::{Result, Store};

pub const USAGE: &str = "kodehub_usage";

/// Estimated USD per million tokens (input, output) for a model family. Pilot estimates; the
/// recorded backend costs nothing.
pub fn price_per_mtok(model: &str) -> (f64, f64) {
    let m = model.to_ascii_lowercase();
    if m.contains("recorded") {
        (0.0, 0.0)
    } else if m.contains("haiku") {
        (1.0, 5.0)
    } else if m.contains("opus") {
        (5.0, 25.0)
    } else {
        (3.0, 15.0)
    }
}

impl Store {
    /// Record one agent call's usage.
    pub async fn record_usage(
        &self,
        project: &str,
        work_item: Option<&str>,
        agent_type: &str,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<Value> {
        let (pi, po) = price_per_mtok(model);
        let cost = (input_tokens as f64 * pi + output_tokens as f64 * po) / 1_000_000.0;
        self.g_insert(
            USAGE,
            &json!({"project": project, "work_item": work_item, "agent_type": agent_type,
                    "model": model, "input_tokens": input_tokens, "output_tokens": output_tokens,
                    "cost_usd": cost, "at": now_iso()}),
        )
        .await
    }

    /// Total tokens and estimated cost for a project.
    pub async fn project_spend(&self, project: &str) -> Result<Value> {
        Ok(self
            .g_aql(
                "LET rows = (FOR u IN @@u FILTER u.project == @p RETURN u)
                 RETURN {project: @p, calls: LENGTH(rows),
                         input_tokens: SUM(rows[*].input_tokens), output_tokens: SUM(rows[*].output_tokens),
                         cost_usd: SUM(rows[*].cost_usd)}",
                json!({"@u": USAGE, "p": project}),
            )
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({"project": project, "cost_usd": 0.0})))
    }
}
