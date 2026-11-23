//! Embedded terminal (doc 05): a PTY on the server bridged to xterm.js over a WebSocket.
//! Client → server: `{"type":"input","data":"..."}` / `{"type":"resize","cols":N,"rows":N}`.
//! Server → client: raw terminal output as text frames.

use std::io::{Read, Write};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::Json;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::{json, Value};

use crate::error::ApiResult;
use crate::AppState;

/// The command that registers Kodehub's MCP server with Claude Code.
pub async fn mcp_command(State(s): State<AppState>) -> ApiResult<Value> {
    let bin = s
        .engine
        .as_ref()
        .map(|e| e.kodehub_bin.clone())
        .unwrap_or_else(|| "kodehub".into());
    let url = s
        .engine
        .as_ref()
        .map(|e| e.server_url.clone())
        .unwrap_or_else(|| "http://127.0.0.1:7878".into());
    let cmd = format!(
        "claude mcp add kodehub --scope local -- {} --server {} mcp",
        bin.display(),
        url
    );
    Ok(Json(json!({"command": cmd})))
}

pub async fn ws(
    ws: WebSocketUpgrade,
    State(s): State<AppState>,
    axum::extract::RawQuery(q): axum::extract::RawQuery,
) -> Response {
    use axum::response::IntoResponse;
    // a shell only for the app itself: the per-launch UI secret is required (final review #8)
    if !crate::from_ui_query(&s, q.as_deref()) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "terminal is only available from the Kodehub app",
        )
            .into_response();
    }
    let url = s
        .engine
        .as_ref()
        .map(|e| e.server_url.clone())
        .unwrap_or_default();
    ws.on_upgrade(move |socket| session(socket, url))
}

async fn session(mut socket: WebSocket, server_url: String) {
    let pty = native_pty_system();
    let pair = match pty.openpty(PtySize {
        rows: 24,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = socket
                .send(Message::Text(
                    format!("terminal unavailable: {e}\r\n").into(),
                ))
                .await;
            return;
        }
    };
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut cmd = CommandBuilder::new(shell);
    cmd.arg("-l");
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("KODEHUB_SERVER_URL", &server_url);
    cmd.env_remove("ANTHROPIC_API_KEY");
    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("cannot start shell: {e}\r\n").into()))
                .await;
            return;
        }
    };
    drop(pair.slave);
    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(_) => return,
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(bytes) => {
                    if socket.send(Message::Text(String::from_utf8_lossy(&bytes).to_string().into())).await.is_err() { break; }
                }
                None => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or(json!({"type": "input", "data": t.to_string()}));
                    match v["type"].as_str() {
                        Some("resize") => {
                            let _ = pair.master.resize(PtySize {
                                rows: v["rows"].as_u64().unwrap_or(24) as u16,
                                cols: v["cols"].as_u64().unwrap_or(100) as u16,
                                pixel_width: 0, pixel_height: 0,
                            });
                        }
                        _ => {
                            if let Some(d) = v["data"].as_str() {
                                let _ = writer.write_all(d.as_bytes());
                                let _ = writer.flush();
                            }
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
    let _ = child.kill();
}
