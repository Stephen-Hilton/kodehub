//! Minimal MCP server over stdio (JSON-RPC 2.0, newline-delimited). Exposes the graph API tool
//! catalog; each call is forwarded to the HTTP API.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::Client;

const SUPPORTED: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub struct McpOptions {
    pub read_only: bool,
    pub allow: Option<BTreeSet<String>>,
    /// Work item key injected into proposal tools (agent sessions).
    pub work_item: Option<String>,
}

pub fn tool_list(opts: &McpOptions) -> Vec<Value> {
    kodehub_core::tools::catalog()
        .into_iter()
        .filter(|t| !opts.read_only || t.read_only)
        .filter(|t| opts.allow.as_ref().map(|a| a.contains(t.name)).unwrap_or(true))
        .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.input_schema}))
        .collect()
}

pub async fn handle(client: &Client, opts: &McpOptions, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => {
            let asked = msg["params"]["protocolVersion"]
                .as_str()
                .unwrap_or(SUPPORTED[0]);
            let v = if SUPPORTED.contains(&asked) {
                asked
            } else {
                SUPPORTED[0]
            };
            Ok(json!({
                "protocolVersion": v,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "kodehub", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Kodehub graph API: code lives as code/test nodes with immutable versions, organised in C4 and project versions, linked to requirements and tests. Use list_requirements, coverage, get_symbol, impact, traverse to read; proposal tools (on a work item) to propose changes.",
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tool_list(opts)})),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("").to_string();
            let mut args = msg["params"]["arguments"].clone();
            if !args.is_object() {
                args = json!({});
            }
            let allowed = tool_list(opts).iter().any(|t| t["name"] == json!(name));
            if !allowed {
                Ok(
                    json!({"content": [{"type": "text", "text": format!("tool `{name}` is not available in this session")}], "isError": true}),
                )
            } else {
                // A session scoped to a work item can only act on that work item's proposal.
                if let (Some(wi), Some(o)) = (&opts.work_item, args.as_object_mut()) {
                    let scoped = o.contains_key("work_item")
                        || name.contains("proposal")
                        || name.starts_with("propose")
                        || name == "submit_review";
                    if scoped {
                        o.insert("work_item".into(), json!(wi));
                    }
                }
                match client.tool(&name, &args).await {
                    Ok(v) => Ok(
                        json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}], "isError": false}),
                    ),
                    Err(e) => Ok(
                        json!({"content": [{"type": "text", "text": e.to_string()}], "isError": true}),
                    ),
                }
            }
        }
        m if m.starts_with("notifications/") => return None,
        other => Err((-32601, format!("method not found: {other}"))),
    };
    let id = id?; // notifications get no response
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    })
}

pub async fn serve_stdio(client: Client, opts: McpOptions) -> anyhow::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}});
                out.write_all(format!("{err}\n").as_bytes()).await?;
                out.flush().await?;
                continue;
            }
        };
        if let Some(resp) = handle(&client, &opts, &msg).await {
            out.write_all(format!("{resp}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(())
}
