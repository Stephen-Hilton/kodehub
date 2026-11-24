//! Kodehub HTTP server: the graph API over HTTP + WebSocket events (doc 01, doc 04).

pub mod auth;
pub mod error;
pub mod events;
pub mod orchestrator;
pub mod routes;

use std::sync::Arc;

use axum::Router;
use kodehub_core::config::Config;
use kodehub_runner::Runner;
use kodehub_store::Store;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;

pub use error::ApiError;
pub use events::Events;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub runner: Runner,
    pub events: Events,
    pub config: Arc<RwLock<Config>>,
    /// Agent engine (orchestrator, coder, reviewer, tester).
    pub engine: Option<Arc<kodehub_agents::Engine>>,
    /// Static UI files to serve at `/` (built app), if any.
    pub static_dir: Option<std::path::PathBuf>,
    /// Browser origins allowed to call the API.
    pub origins: Arc<Vec<String>>,
    /// Per-launch secret given only to the UI; required on every human-only route (final #8).
    pub ui_token: Arc<String>,
    /// One-time prod approvals issued to the UI: id → (release, issued).
    pub approvals:
        Arc<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>>,
}

impl AppState {
    pub fn new(store: Store, runner: Runner, config: Config) -> Self {
        AppState {
            store,
            runner,
            events: Events::new(),
            config: Arc::new(RwLock::new(config)),
            engine: None,
            static_dir: None,
            origins: Arc::new(default_origins()),
            ui_token: Arc::new(format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )),
            approvals: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
}

/// Default browser origins: the Tauri app and the Vite dev / e2e ports. More can be added with
/// `KODEHUB_UI_ORIGINS` (comma-separated); the server's own origin is added at startup.
pub fn default_origins() -> Vec<String> {
    let mut v: Vec<String> = [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for port in [1420, 1421, 1431] {
        v.push(format!("http://localhost:{port}"));
        v.push(format!("http://127.0.0.1:{port}"));
    }
    if let Ok(extra) = std::env::var("KODEHUB_UI_ORIGINS") {
        v.extend(
            extra
                .split(',')
                .map(|s| s.trim().trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty()),
        );
    }
    v
}

/// Exact-match origin allowlist (review M6 #16: not every localhost port).
pub fn origin_allowed(allowed: &[String], origin: &str) -> bool {
    let o = origin.trim_end_matches('/');
    allowed.iter().any(|a| a == o)
}

/// Extra Host names to serve (`KODEHUB_ALLOWED_HOSTS`, comma-separated): in-cluster service
/// names such as `kodehub-server`.
fn extra_hosts() -> &'static Vec<String> {
    static H: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        std::env::var("KODEHUB_ALLOWED_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Only loopback Host headers (and configured service names) are served (defeats DNS
/// rebinding).
pub fn host_allowed(host: &str) -> bool {
    let h = if host.starts_with('[') {
        host.split(']')
            .next()
            .map(|x| format!("{x}]"))
            .unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    matches!(h.as_str(), "localhost" | "127.0.0.1" | "[::1]")
        || extra_hosts().iter().any(|x| *x == h.to_ascii_lowercase())
}

/// True when the request carries the per-launch UI secret (`x-kodehub-ui` header, or `ui=`
/// query parameter for WebSockets, which cannot set headers). Human-only actions require it:
/// reviews that count, plan approval, escalations, prod approvals, chat Apply, requirement edits,
/// rollback, settings and the terminal. MCP/CLI/agent requests never have it.
pub fn from_ui(state: &AppState, headers: &axum::http::HeaderMap) -> bool {
    let presented = headers
        .get("x-kodehub-ui")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    constant_eq(presented.as_bytes(), state.ui_token.as_bytes())
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 403 unless the caller is the UI.
pub fn require_ui(state: &AppState, headers: &axum::http::HeaderMap) -> Result<(), ApiError> {
    if from_ui(state, headers) {
        Ok(())
    } else {
        Err(ApiError {
            status: axum::http::StatusCode::FORBIDDEN,
            kind: "human_only",
            message: "this action is reserved for a human in the Kodehub app".into(),
        })
    }
}

/// Serve index.html (sign-in happens in the UI; no secret is injected).
async fn index_with_token(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(dir) = state.static_dir.clone() else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    match std::fs::read_to_string(dir.join("index.html")) {
        Ok(html) => axum::response::Html(html).into_response(),
        Err(_) => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

async fn origin_guard(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    // liveness/readiness probes come by pod IP; health reveals nothing and changes nothing
    let probe = req.method() == axum::http::Method::GET && req.uri().path() == "/health";
    if let Some(h) = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        if !probe && !host_allowed(h) {
            return (axum::http::StatusCode::FORBIDDEN, "host not allowed").into_response();
        }
    }
    if let Some(o) = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        if !origin_allowed(&state.origins, o) {
            return (axum::http::StatusCode::FORBIDDEN, "origin not allowed").into_response();
        }
    }
    next.run(req).await
}

pub fn router(state: AppState) -> Router {
    use tower_http::cors::{AllowOrigin, Any};
    let static_dir = state.static_dir.clone();
    let state_for_index = state.clone();
    let origins = state.origins.clone();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |o, _| {
            o.to_str()
                .map(|o| origin_allowed(&origins, o))
                .unwrap_or(false)
        }))
        .allow_methods(Any)
        .allow_headers(Any);
    let mut r = routes::api()
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_guard,
        ))
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(state, origin_guard));
    if let Some(dir) = static_dir {
        let index = axum::routing::get(index_with_token).with_state(state_for_index);
        r = r
            .route_service("/", index.clone())
            .route_service("/index.html", index.clone())
            .fallback_service(tower_http::services::ServeDir::new(&dir).fallback(index));
    }
    r
}

/// Serve on an already-bound listener (used by main and tests).
pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(listener, router(state)).await
}

/// Build the agent engine and attach it to the state.
pub fn install_agents(state: &mut AppState, server_url: &str, opts: kodehub_agents::EngineOptions) {
    let events = state.events.clone();
    let emit: kodehub_agents::Emitter =
        Arc::new(move |kind: &str, data: serde_json::Value| events.emit(kind, data));
    let wrapper = kodehub_agents::backend::claude_code::install_wrapper(&state.runner.home)
        .unwrap_or_else(|_| std::path::PathBuf::from("scripts/claude-as.sh"));
    let kodehub_bin = std::env::var_os("KODEHUB_BIN")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            let dir = exe.parent()?;
            [dir.join("kodehub"), dir.join("../kodehub")]
                .into_iter()
                .find(|p| p.exists())
        })
        .unwrap_or_else(|| std::path::PathBuf::from("kodehub"));
    let engine = kodehub_agents::Engine::new(
        state.store.clone(),
        state.runner.clone(),
        state.config.clone(),
        server_url.to_string(),
        kodehub_bin,
        wrapper,
        emit,
        opts,
    );
    state.engine = Some(Arc::new(engine));
}

impl AppState {
    /// A requirement was edited by a human: open + plan a changeset in the background.
    pub fn requirement_edited(&self, key: &str, by: &str) {
        if let Some(e) = self.engine.clone() {
            let (k, b, ev) = (key.to_string(), by.to_string(), self.events.clone());
            kodehub_store::spawn_scoped(async move {
                if let Err(err) = e.on_requirement_edited(&k, &b).await {
                    ev.emit("agent.activity", serde_json::json!({"role": "orchestrator", "error": err.to_string(), "requirement": k}));
                }
            });
        }
    }

    /// Run (or resume) a changeset's workflow in the background.
    pub fn execute_changeset(&self, key: &str) {
        if let Some(e) = self.engine.clone() {
            let (k, ev) = (key.to_string(), self.events.clone());
            kodehub_store::spawn_scoped(async move {
                if let Err(err) = e.execute(&k).await {
                    ev.emit("agent.activity", serde_json::json!({"role": "orchestrator", "error": err.to_string(), "changeset": k}));
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origins_and_hosts() {
        let allowed = default_origins();
        let ok = |o: &str| origin_allowed(&allowed, o);
        assert!(ok("http://localhost:1420"));
        assert!(ok("tauri://localhost"));
        assert!(
            !ok("http://localhost:9999"),
            "arbitrary local ports are not trusted"
        );
        assert!(!ok("https://evil.example"));
        assert!(!ok("null"));
        assert!(host_allowed("127.0.0.1:7878"));
        assert!(host_allowed("localhost"));
        assert!(host_allowed("[::1]:7878"));
        assert!(!host_allowed("evil.example:7878"));
        assert!(!host_allowed("127.0.0.1.nip.io"));
    }
}
