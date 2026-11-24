//! Recorded backend: replays stored responses (deterministic tests and CI) and records every
//! task it receives so tests can assert on prompts (e.g. blind review isolation).
//!
//! Recording file: `{"scenario": "...", "responses": [{"role": "coder", "label": "...", "output": {...}}]}`.
//! Responses are consumed in order per role.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::{AgentBackend, AgentError, AgentOutput, AgentTask, Result, Role};

pub struct RecordedBackend {
    queues: Mutex<BTreeMap<Role, VecDeque<Value>>>,
    calls: Mutex<Vec<AgentTask>>,
    pub scenario: String,
}

impl RecordedBackend {
    pub fn from_value(v: &Value) -> Result<Self> {
        let mut queues: BTreeMap<Role, VecDeque<Value>> = BTreeMap::new();
        for r in v["responses"].as_array().cloned().unwrap_or_default() {
            let role: Role = serde_json::from_value(r["role"].clone())
                .map_err(|e| AgentError::Backend(format!("recording role: {e}")))?;
            queues
                .entry(role)
                .or_default()
                .push_back(r["output"].clone());
        }
        Ok(RecordedBackend {
            queues: Mutex::new(queues),
            calls: Mutex::new(Vec::new()),
            scenario: v["scenario"].as_str().unwrap_or("recorded").into(),
        })
    }

    pub fn from_file(p: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(p)
            .map_err(|e| AgentError::Backend(format!("{}: {e}", p.display())))?;
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| AgentError::Backend(format!("{}: {e}", p.display())))?;
        Self::from_value(&v)
    }

    /// Every task received so far.
    pub fn calls(&self) -> Vec<AgentTask> {
        self.calls.lock().unwrap().clone()
    }

    pub fn remaining(&self) -> usize {
        self.queues.lock().unwrap().values().map(|q| q.len()).sum()
    }
}

#[async_trait::async_trait]
impl AgentBackend for RecordedBackend {
    fn name(&self) -> &str {
        "recorded"
    }

    async fn run(&self, task: &AgentTask) -> Result<AgentOutput> {
        self.calls.lock().unwrap().push(task.clone());
        let out = self
            .queues
            .lock()
            .unwrap()
            .get_mut(&task.role)
            .and_then(|q| q.pop_front())
            .ok_or_else(|| {
                AgentError::Backend(format!(
                    "recording '{}' has no more {} responses (task: {})",
                    self.scenario,
                    task.role.as_str(),
                    task.label
                ))
            })?;
        Ok(AgentOutput {
            text: out.to_string(),
            json: out,
            transcript: vec![
                json!({"kind": "text", "role": "assistant", "text": "(recorded response)"}),
            ],
            backend: "recorded".into(),
            model: format!("recorded:{}", self.scenario),
            duration_ms: 0,
            usage: json!({"input_tokens": 0, "output_tokens": 0}),
        })
    }
}
