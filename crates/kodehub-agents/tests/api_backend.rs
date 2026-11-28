//! API backend against a mock Messages API: headers, tool loop, structured output, key rules.
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use kodehub_agents::{schemas, AgentBackend, AgentTask, ApiBackend, Role};
use serde_json::{json, Value};

#[derive(Clone, Default)]
struct Seen {
    requests: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
}

async fn messages(
    State(s): State<Seen>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let n = {
        let mut r = s.requests.lock().unwrap();
        r.push((headers, body.clone()));
        r.len()
    };
    if n == 1 {
        Json(
            json!({"id": "m1", "type": "message", "role": "assistant", "stop_reason": "tool_use",
            "content": [{"type": "text", "text": "Let me look."},
                        {"type": "tool_use", "id": "tu1", "name": "list_requirements", "input": {}}]}),
        )
    } else {
        Json(
            json!({"id": "m2", "type": "message", "role": "assistant", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "{\"verdict\":\"approve\",\"summary\":\"ok\",\"findings\":[]}"}]}),
        )
    }
}

async fn requirements() -> Json<Value> {
    Json(json!({"requirements": [{"_key": "r1", "title": "R1"}]}))
}

#[tokio::test]
async fn api_backend_runs_tool_loop_with_correct_headers() {
    let seen = Seen::default();
    let app = Router::new()
        .route("/v1/messages", post(messages))
        .route("/api/requirements", get(requirements))
        .with_state(seen.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let b = ApiBackend::new("sk-ant-api03-test", &url, &url).unwrap();
    let task = AgentTask {
        role: Role::Reviewer,
        system: "be blind".into(),
        input: "review this".into(),
        schema: schemas::review(),
        model: "claude-opus-5-5".into(),
        work_item: None,
        label: "t".into(),
        api_token: None,
        project: None,
    };
    let out = b.run(&task).await.unwrap();
    assert_eq!(out.json["verdict"], "approve");
    assert!(out
        .transcript
        .iter()
        .any(|t| t["kind"] == "tool_use" && t["name"] == "list_requirements"));
    let reqs = seen.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    let (h, body) = &reqs[0];
    assert_eq!(h["x-api-key"], "sk-ant-api03-test");
    assert_eq!(h["anthropic-version"], "2026-10-21");
    assert!(h.get("authorization").is_none());
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    // reviewer only gets its narrow read-only tool set
    let tools: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, vec!["list_requirements"]);
    // tool result was fed back
    let second = &reqs[1].1["messages"];
    assert_eq!(second[2]["content"][0]["type"], "tool_result");
    assert!(second[2]["content"][0]["content"]
        .as_str()
        .unwrap()
        .contains("R1"));
}

#[test]
fn subscription_tokens_are_never_sent_to_the_messages_api() {
    assert!(ApiBackend::new("sk-ant-oat01-abc", "http://x", "http://x").is_err());
}

#[test]
fn claude_code_args_disable_builtin_tools_and_restrict_mcp() {
    let b = kodehub_agents::ClaudeCodeBackend {
        wrapper: "w".into(),
        token_env: "DEV1_TOKEN".into(),
        kodehub_bin: "/bin/kodehub".into(),
        server_url: "http://s".into(),
        workdir: "/tmp".into(),
        timeout: std::time::Duration::from_secs(1),
        oauth_token: None,
    };
    let task = AgentTask {
        role: Role::Tester,
        system: "sys".into(),
        input: "in".into(),
        schema: json!({}),
        model: "m".into(),
        work_item: Some("wi-1".into()),
        label: "l".into(),
        api_token: None,
        project: None,
    };
    let a = b.args(&task);
    let pos = |f: &str| a.iter().position(|x| x == f).unwrap();
    assert_eq!(a[pos("--tools") + 1], "");
    assert_eq!(a[pos("--permission-mode") + 1], "dontAsk");
    assert!(a.contains(&"--strict-mcp-config".to_string()));
    assert!(a.contains(&"--no-session-persistence".to_string()));
    assert_eq!(a.last().unwrap(), "mcp__kodehub__list_requirements");
    let mcp: Value = serde_json::from_str(&a[pos("--mcp-config") + 1]).unwrap();
    let margs: Vec<&str> = mcp["mcpServers"]["kodehub"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(margs.contains(&"--read-only"));
    assert!(
        !a.iter().any(|x| x.contains("DEV1_TOKEN")),
        "token var is passed to the wrapper, not claude"
    );
}
