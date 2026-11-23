#![allow(dead_code)]
use std::path::PathBuf;

use kodehub_core::config::Config;
use kodehub_runner::Runner;
use kodehub_server::AppState;
use kodehub_store::testutil::temp_store;
use kodehub_store::Store;
use serde_json::Value;

pub fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

pub fn runner() -> Runner {
    let base_dir = repo().join(".kode/test");
    let mut r = Runner::new(base_dir.join(format!("home-{}", uuid::Uuid::new_v4().simple())));
    r.cargo_target = base_dir.join("cargo-target");
    r.pyenv = base_dir.join("pyenv");
    std::fs::create_dir_all(&r.home).unwrap();
    r
}

pub struct TestServer {
    pub url: String,
    pub store: Store,
    pub state: AppState,
    http: reqwest::Client,
}

impl TestServer {
    pub async fn start(prefix: &str) -> TestServer {
        Self::start_with(prefix, |_, _| {}).await
    }

    pub async fn start_with(prefix: &str, f: impl FnOnce(&mut AppState, &str)) -> TestServer {
        let store = temp_store(prefix).await.expect("arangodb reachable");
        let mut cfg = Config::default();
        cfg.agents.backend = "recorded".into();
        let mut state = AppState::new(store.clone(), runner(), cfg);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        f(&mut state, &url);
        let st = state.clone();
        tokio::spawn(async move { kodehub_server::serve(listener, st).await.unwrap() });
        TestServer {
            url,
            store,
            state,
            http: reqwest::Client::new(),
        }
    }

    pub async fn req(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut r = self.http.request(m, format!("{}{}", self.url, path));
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    /// Request as the desktop UI (allowed browser Origin) — needed for human-only actions.
    pub async fn ui(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut r = self
            .http
            .request(m, format!("{}{}", self.url, path))
            .header("Origin", "http://localhost:1420")
            .header("x-kodehub-ui", self.state.ui_token.as_str());
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    /// `ui()` that must succeed.
    pub async fn uok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.ui(method, path, body).await;
        assert!((200..300).contains(&s), "{method} {path} (ui) → {s}: {v}");
        v
    }

    pub async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.req(method, path, body).await;
        assert!((200..300).contains(&s), "{method} {path} → {s}: {v}");
        v
    }

    pub async fn stop(self) {
        self.store.drop_db().await.unwrap();
    }
}

pub fn seed_path() -> String {
    repo().join("examples/auth-poc").to_string_lossy().into()
}
