//! API backend: Messages API over raw HTTP (Rust has no official SDK) with a manual tool-use
//! loop. Authenticates only with `KODEHUB_ANTHROPIC_API_KEY` (a Console key) — never
//! `ANTHROPIC_API_KEY`, never a subscription OAuth token. Tools are the same read-only graph
//! tools the MCP server exposes, executed against the Kodehub HTTP API.

use std::time::Instant;

use serde_json::{json, Value};

use crate::{extract_json, AgentBackend, AgentError, AgentOutput, AgentTask, Result};

pub const API_KEY_ENV: &str = "KODEHUB_ANTHROPIC_API_KEY";
pub const API_VERSION: &str = "2026-10-21";

pub struct ApiBackend {
    pub base_url: String,
    pub api_key: String,
    pub server_url: String,
    http: reqwest::Client,
    pub max_iterations: usize,
}

impl ApiBackend {
    /// Build from the environment. Refuses subscription OAuth tokens.
    pub fn from_env(server_url: &str) -> Result<Self> {
        let key = std::env::var(API_KEY_ENV).map_err(|_| {
            AgentError::Backend(format!(
                "{API_KEY_ENV} is not set (the API backend needs a Console API key)"
            ))
        })?;
        Self::new(&key, server_url, "https://api.anthropic.com")
    }

    pub fn new(api_key: &str, server_url: &str, base_url: &str) -> Result<Self> {
        if api_key.starts_with("sk-ant-oat") {
            return Err(AgentError::Backend("refusing to send a subscription OAuth token to the Messages API; use the Claude Code backend".into()));
        }
        Ok(ApiBackend {
            base_url: base_url.trim_end_matches('/').into(),
            api_key: api_key.into(),
            server_url: server_url.trim_end_matches('/').into(),
            http: reqwest::Client::new(),
            max_iterations: 16,
        })
    }

    fn tools(&self, task: &AgentTask) -> Vec<Value> {
        kodehub_core::tools::catalog()
            .into_iter()
            .filter(|t| task.role.tools().contains(&t.name) && t.read_only)
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema}))
            .collect()
    }

    async fn exec_tool(&self, name: &str, input: &Value, task: &AgentTask) -> (String, bool) {
        if !task.role.tools().contains(&name) {
            return (
                format!(
                    "tool {name} is not available to the {} agent",
                    task.role.as_str()
                ),
                true,
            );
        }
        let defs = kodehub_core::tools::catalog();
        let Some(def) = defs.iter().find(|d| d.name == name) else {
            return (format!("unknown tool {name}"), true);
        };
        let (m, p, b) = kodehub_core::tools::to_request(def, input);
        let mut req = self.http.request(
            reqwest::Method::from_bytes(m.as_bytes()).unwrap(),
            format!("{}{}", self.server_url, p),
        );
        if let Some(b) = b {
            req = req.json(&b);
        }
        match req.send().await {
            Ok(r) => {
                let ok = r.status().is_success();
                let t = r.text().await.unwrap_or_default();
                (t.chars().take(60_000).collect(), !ok)
            }
            Err(e) => (e.to_string(), true),
        }
    }
}

#[async_trait::async_trait]
impl AgentBackend for ApiBackend {
    fn name(&self) -> &str {
        "api"
    }

    async fn run(&self, task: &AgentTask) -> Result<AgentOutput> {
        let start = Instant::now();
        let tools = self.tools(task);
        let mut messages = vec![json!({"role": "user", "content": task.input})];
        let mut transcript = Vec::new();
        for _ in 0..self.max_iterations {
            let mut body = json!({
                "model": task.model,
                "max_tokens": 16000,
                "system": task.system,
                "messages": messages,
                "output_config": {"format": {"type": "json_schema", "schema": task.schema}},
            });
            if !tools.is_empty() {
                body["tools"] = json!(tools);
            }
            let resp = self
                .http
                .post(format!("{}/v1/messages", self.base_url))
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| AgentError::Backend(e.to_string()))?;
            let status = resp.status();
            let v: Value = resp
                .json()
                .await
                .map_err(|e| AgentError::Backend(e.to_string()))?;
            if !status.is_success() {
                return Err(AgentError::Backend(format!(
                    "Messages API {status}: {}",
                    v["error"]["message"].as_str().unwrap_or(&v.to_string())
                )));
            }
            let stop = v["stop_reason"].as_str().unwrap_or("");
            if stop == "refusal" {
                return Err(AgentError::Backend(format!(
                    "model refused: {}",
                    v["stop_details"]
                )));
            }
            let content = v["content"].as_array().cloned().unwrap_or_default();
            messages.push(json!({"role": "assistant", "content": content}));
            if stop == "tool_use" {
                let mut results = Vec::new();
                for c in content.iter().filter(|c| c["type"] == "tool_use") {
                    let name = c["name"].as_str().unwrap_or("");
                    transcript.push(json!({"kind": "tool_use", "name": name, "input": c["input"]}));
                    let (text, is_error) = self.exec_tool(name, &c["input"], task).await;
                    transcript.push(json!({"kind": "tool_result", "text": text.chars().take(2000).collect::<String>()}));
                    results.push(json!({"type": "tool_result", "tool_use_id": c["id"], "content": text, "is_error": is_error}));
                }
                messages.push(json!({"role": "user", "content": results}));
                continue;
            }
            if stop == "max_tokens" {
                return Err(AgentError::Backend("response hit max_tokens".into()));
            }
            let text: String = content
                .iter()
                .filter(|c| c["type"] == "text")
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("");
            transcript.push(json!({"kind": "text", "role": "assistant", "text": text}));
            let json = extract_json(&text).ok_or_else(|| {
                AgentError::Output(format!(
                    "no JSON in response: {}",
                    text.chars().take(400).collect::<String>()
                ))
            })?;
            return Ok(AgentOutput {
                json,
                text,
                transcript,
                backend: "api".into(),
                model: task.model.clone(),
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }
        Err(AgentError::Backend(
            "tool loop exceeded max iterations".into(),
        ))
    }
}
