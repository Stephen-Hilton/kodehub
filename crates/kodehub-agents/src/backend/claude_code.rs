//! Claude Code backend (default): drives the official `claude` CLI headless with the developer's
//! own login (or a long-lived token chosen by env var *name*, see decisions.md D-021). Built-in
//! tools are disabled; only read-only `mcp__kodehub__*` tools are allowed. One fresh session per
//! task (`--no-session-persistence`). Flags verified in decisions.md D-020.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::{extract_json, AgentBackend, AgentError, AgentOutput, AgentTask, Result};

pub struct ClaudeCodeBackend {
    /// `scripts/claude-as.sh` (written to $KODEHUB_HOME/bin at startup).
    pub wrapper: PathBuf,
    /// Env var NAME holding a Claude Code token, or empty for the CLI's own login.
    pub token_env: String,
    /// `kodehub` CLI binary used as the MCP server.
    pub kodehub_bin: PathBuf,
    pub server_url: String,
    pub workdir: PathBuf,
    pub timeout: Duration,
}

pub const WRAPPER_SCRIPT: &str = include_str!("../../../../scripts/claude-as.sh");

/// Write the wrapper into `$KODEHUB_HOME/bin/claude-as.sh` and return its path.
pub fn install_wrapper(home: &std::path::Path) -> std::io::Result<PathBuf> {
    let dir = home.join("bin");
    std::fs::create_dir_all(&dir)?;
    let p = dir.join("claude-as.sh");
    std::fs::write(&p, WRAPPER_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(p)
}

impl ClaudeCodeBackend {
    pub fn args(&self, task: &AgentTask) -> Vec<String> {
        let mut mcp_args = vec![
            "--server".to_string(),
            self.server_url.clone(),
            "mcp".into(),
            "--read-only".into(),
            "--tools".into(),
            task.role.tools().join(","),
        ];
        if let Some(cs) = &task.changeset {
            mcp_args.push("--changeset".into());
            mcp_args.push(cs.clone());
        }
        let mcp =
            json!({"mcpServers": {"kodehub": {"command": self.kodehub_bin, "args": mcp_args}}});
        // the prompt goes over stdin, never argv (visible in `ps`, limited by ARG_MAX)
        let mut a: Vec<String> = vec![
            "-p".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--mcp-config".into(),
            mcp.to_string(),
            "--strict-mcp-config".into(),
            "--tools".into(),
            "".into(),
            "--permission-mode".into(),
            "dontAsk".into(),
            "--setting-sources".into(),
            "".into(),
            "--no-session-persistence".into(),
            "--append-system-prompt".into(),
            task.system.clone(),
            "--model".into(),
            task.model.clone(),
            "--json-schema".into(),
            task.schema.to_string(),
        ];
        a.push("--allowedTools".into());
        for t in task.role.tools() {
            a.push(format!("mcp__kodehub__{t}"));
        }
        a
    }
}

#[async_trait::async_trait]
impl AgentBackend for ClaudeCodeBackend {
    fn name(&self) -> &str {
        "claude_code"
    }

    async fn run(&self, task: &AgentTask) -> Result<AgentOutput> {
        let start = Instant::now();
        std::fs::create_dir_all(&self.workdir).map_err(|e| AgentError::Backend(e.to_string()))?;
        let token = if self.token_env.is_empty() {
            "-".to_string()
        } else {
            self.token_env.clone()
        };
        let mut cmd = tokio::process::Command::new(&self.wrapper);
        cmd.arg(&token)
            .args(self.args(task))
            .current_dir(&self.workdir)
            .env_remove("ANTHROPIC_API_KEY")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| AgentError::Backend(format!("cannot start claude: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(task.input.as_bytes())
                .await
                .map_err(|e| AgentError::Backend(e.to_string()))?;
            drop(stdin);
        }
        let out = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                AgentError::Backend(format!(
                    "{} timed out after {:?}",
                    task.role.as_str(),
                    self.timeout
                ))
            })?
            .map_err(|e| AgentError::Backend(format!("claude failed: {e}")))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut transcript = Vec::new();
        let mut result: Option<Value> = None;
        for line in stdout.lines() {
            let Ok(ev) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match ev["type"].as_str() {
                Some("assistant") | Some("user") => {
                    for c in ev["message"]["content"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                    {
                        match c["type"].as_str() {
                            Some("text") => transcript.push(
                                json!({"kind": "text", "role": ev["type"], "text": c["text"]}),
                            ),
                            Some("tool_use") => transcript.push(
                                json!({"kind": "tool_use", "name": c["name"], "input": c["input"]}),
                            ),
                            Some("tool_result") => {
                                let text = c["content"]
                                    .as_array()
                                    .and_then(|a| a.first())
                                    .and_then(|x| x["text"].as_str())
                                    .map(String::from)
                                    .or_else(|| c["content"].as_str().map(String::from))
                                    .unwrap_or_default();
                                let short: String = text.chars().take(2000).collect();
                                transcript.push(json!({"kind": "tool_result", "text": short}));
                            }
                            _ => {}
                        }
                    }
                }
                Some("result") => result = Some(ev),
                _ => {}
            }
        }
        let Some(res) = result else {
            return Err(AgentError::Backend(format!(
                "claude produced no result (exit {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(800)
                    .collect::<String>()
            )));
        };
        if res["is_error"] == json!(true) {
            return Err(AgentError::Backend(format!(
                "claude error: {}",
                res["result"].as_str().unwrap_or(&res.to_string())
            )));
        }
        let text = res["result"].as_str().unwrap_or("").to_string();
        let json = res
            .get("structured_output")
            .filter(|v| v.is_object())
            .cloned()
            .or_else(|| extract_json(&text))
            .ok_or_else(|| {
                AgentError::Output(format!(
                    "no JSON in result: {}",
                    text.chars().take(400).collect::<String>()
                ))
            })?;
        Ok(AgentOutput {
            json,
            text,
            transcript,
            backend: "claude_code".into(),
            model: task.model.clone(),
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }
}
