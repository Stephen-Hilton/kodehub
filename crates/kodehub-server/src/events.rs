//! WebSocket event bus: node.changed, changeset.updated, test.result, run.output,
//! agent.activity, env.deployed (doc 01).

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::AppState;

#[derive(Clone)]
pub struct Events {
    tx: broadcast::Sender<Value>,
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

impl Events {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(2048);
        Events { tx }
    }
    pub fn emit(&self, kind: &str, data: Value) {
        let _ = self.tx.send(
            json!({"type": kind, "data": data, "at": kodehub_core::model::now_iso(),
                         "project": kodehub_store::request_project()}),
        );
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }
}

/// Events for the subscriber's project only (plus project-less system events).
pub async fn ws_events(ws: WebSocketUpgrade, State(s): State<AppState>) -> Response {
    let project = kodehub_store::request_project();
    ws.on_upgrade(move |socket| pump(socket, s.events.subscribe(), project))
}

async fn pump(mut socket: WebSocket, mut rx: broadcast::Receiver<Value>, project: Option<String>) {
    let _ = socket
        .send(Message::Text(
            json!({"type": "hello", "data": {}}).to_string().into(),
        ))
        .await;
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(v) => {
                    let p = v["project"].as_str();
                    if p.is_some() && p != project.as_deref() { continue; }
                    if socket.send(Message::Text(v.to_string().into())).await.is_err() { break; }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
}
