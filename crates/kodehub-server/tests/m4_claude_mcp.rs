//! M4 acceptance (live): real Claude Code connected via MCP lists requirements and runs coverage.
//! Runs only when KODEHUB_LIVE_TOKEN_ENV names a set env var holding a Claude Code token
//! (e.g. KODEHUB_LIVE_TOKEN_ENV=DEV1_TOKEN). Uses the developer's subscription, never an API key.
mod common;

use common::*;
use serde_json::{json, Value};

#[tokio::test]
async fn claude_code_lists_requirements_and_runs_coverage_over_mcp() {
    let Ok(var) = std::env::var("KODEHUB_LIVE_TOKEN_ENV") else {
        eprintln!("skipped: KODEHUB_LIVE_TOKEN_ENV not set");
        return;
    };
    if std::env::var_os(&var).is_none() {
        eprintln!("skipped: {var} not set");
        return;
    }
    let t = TestServer::start("claude").await;
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    t.ok(
        "POST",
        "/api/tests/run",
        Some(json!({"scope": {"kind": "all"}})),
    )
    .await;
    let bin = repo().join("target/debug/kodehub");
    let mcp = json!({"mcpServers": {"kodehub": {"command": bin, "args": ["--server", t.url, "mcp", "--read-only"]}}});
    let cwd = repo().join(".kode/test/claude-cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let out = tokio::process::Command::new(repo().join("scripts/claude-as.sh"))
        .arg(&var)
        .args(["-p", "Use the kodehub MCP tools: first call list_requirements, then call coverage. \
                     Then reply with exactly one line per requirement in the form `<key>: <status>` and nothing else.",
               "--output-format", "stream-json", "--verbose",
               "--mcp-config", &mcp.to_string(), "--strict-mcp-config",
               "--tools", "",
               "--allowedTools", "mcp__kodehub__list_requirements", "mcp__kodehub__coverage",
               "--permission-mode", "dontAsk",
               "--setting-sources", "",
               "--no-session-persistence",
               "--model", "claude-haiku-5-5"])
        .current_dir(&cwd)
        .output()
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let events: Vec<Value> = stdout
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let tool_uses: Vec<String> = events
        .iter()
        .filter(|e| e["type"] == "assistant")
        .flat_map(|e| {
            e["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|c| c["type"] == "tool_use")
        .map(|c| c["name"].as_str().unwrap_or("").to_string())
        .collect();
    let result = events
        .iter()
        .find(|e| e["type"] == "result")
        .cloned()
        .unwrap_or(json!({}));
    assert!(
        tool_uses.contains(&"mcp__kodehub__list_requirements".to_string()),
        "tools used: {tool_uses:?}\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        tool_uses.contains(&"mcp__kodehub__coverage".to_string()),
        "tools used: {tool_uses:?}"
    );
    let text = result["result"].as_str().unwrap_or("");
    assert!(
        text.contains("r1-generic-failure") && text.contains("pass"),
        "final answer: {text}"
    );
    t.stop().await;
}
