//! Agents (doc 04): orchestrator, coder, reviewer, tester, environment agents, running on a
//! pluggable backend. Agents read the graph through MCP tools and return one structured JSON
//! result; the engine applies results through the graph API (decisions.md D-025).

pub mod actions;
pub mod backend;
pub mod context;
pub mod engine;
pub mod environments;
pub mod prompts;
pub mod roles;
pub mod schemas;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use backend::{api::ApiBackend, claude_code::ClaudeCodeBackend, recorded::RecordedBackend};
pub use engine::{Engine, EngineOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Orchestrator,
    Coder,
    Reviewer,
    Tester,
    Environment,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Orchestrator => "orchestrator",
            Role::Coder => "coder",
            Role::Reviewer => "reviewer",
            Role::Tester => "tester",
            Role::Environment => "environment",
        }
    }
    pub const ALL: [Role; 4] = [
        Role::Orchestrator,
        Role::Tester,
        Role::Coder,
        Role::Reviewer,
    ];

    /// Read-only graph tools this role may call. The reviewer and tester get narrow sets so
    /// they cannot read the coder's transcripts (blind review) or function bodies (tester).
    pub fn tools(&self) -> &'static [&'static str] {
        match self {
            Role::Orchestrator => &[
                "get_node",
                "query_nodes",
                "traverse",
                "get_symbol",
                "list_requirements",
                "coverage",
                "orphans",
                "impact",
                "duplicates",
            ],
            Role::Coder => &[
                "get_symbol",
                "traverse",
                "impact",
                "list_requirements",
                "coverage",
            ],
            Role::Reviewer => &["list_requirements"],
            Role::Tester => &["list_requirements"],
            Role::Environment => &["coverage", "orphans"],
        }
    }
}

/// One unit of agent work. The engine builds `input` from the graph; for the reviewer it is
/// built *only* from requirements, mission, diff and tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub role: Role,
    /// Role prompt (appended to the system prompt).
    pub system: String,
    /// Task input.
    pub input: String,
    /// JSON schema the final answer must match.
    pub schema: Value,
    pub model: String,
    pub changeset: Option<String>,
    /// Label for logs, e.g. "plan", "review round 1".
    pub label: String,
    /// Short-lived Kodehub token for the agent's read-only graph tools (MCP / HTTP), scoped to
    /// one project and work item. Never a model credential.
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub json: Value,
    pub text: String,
    /// Tool calls and messages (for transcripts).
    pub transcript: Vec<Value>,
    pub backend: String,
    pub model: String,
    pub duration_ms: u64,
    /// Token usage reported by the backend (`input_tokens`, `output_tokens`).
    #[serde(default)]
    pub usage: Value,
}

tokio::task_local! {
    /// The work item an agent call belongs to (metering, tokens).
    pub static CURRENT_WORK_ITEM: String;
}

pub fn current_work_item() -> Option<String> {
    CURRENT_WORK_ITEM.try_with(|w| w.clone()).ok()
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent backend error: {0}")]
    Backend(String),
    #[error("agent returned invalid output: {0}")]
    Output(String),
    #[error(transparent)]
    Store(#[from] kodehub_store::StoreError),
    #[error(transparent)]
    Runner(#[from] kodehub_runner::RunnerError),
    #[error("{0}")]
    Workflow(String),
    /// An external blocker only a human can clear (spec §4.5): no credential, revoked key.
    #[error("blocked: {0}")]
    Blocked(String),
}

pub type Result<T> = std::result::Result<T, AgentError>;

#[async_trait::async_trait]
pub trait AgentBackend: Send + Sync {
    fn name(&self) -> &str;
    async fn run(&self, task: &AgentTask) -> Result<AgentOutput>;
}

pub type Emitter = Arc<dyn Fn(&str, Value) + Send + Sync>;

/// Extract a JSON object from model text (handles ```json fences and leading prose).
pub fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        if v.is_object() {
            return Some(v);
        }
    }
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    serde_json::from_str(&t[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_extraction() {
        assert_eq!(extract_json("```json\n{\"a\": 1}\n```").unwrap()["a"], 1);
        assert_eq!(
            extract_json("Here you go: {\"a\": {\"b\": 2}} done").unwrap()["a"]["b"],
            2
        );
        assert!(extract_json("no json").is_none());
    }
}
