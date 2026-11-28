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
    /// Session token of the signed-in human (`tester`, the system admin).
    pub session: String,
    /// Personal access token of an external agent user acting for `tester`.
    pub pat: String,
}

impl TestServer {
    /// A key or id qualified for the store's active project (`x` → `<project>.x`,
    /// `symbols/x` → `symbols/<project>.x`).
    pub fn q(&self, x: &str) -> String {
        if x.contains('/') {
            self.store.db.qid(x).unwrap()
        } else {
            self.store.db.pkey(x).unwrap()
        }
    }

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
        let http = reqwest::Client::new();
        let reg: Value = http
            .post(format!("{url}/api/auth/register"))
            .json(&serde_json::json!({"username": "tester", "password": "password123"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let session = reg["token"].as_str().expect("registered").to_string();
        let agent: Value = http
            .post(format!("{url}/api/agent-users"))
            .bearer_auth(&session)
            .json(&serde_json::json!({"name": "mcp"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let tok: Value = http
            .post(format!("{url}/api/tokens"))
            .bearer_auth(&session)
            .json(
                &serde_json::json!({"agent": agent["key"], "name": "tests", "permission": "write"}),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let pat = tok["token"].as_str().expect("token issued").to_string();
        TestServer {
            url,
            store,
            state,
            http,
            session,
            pat,
        }
    }

    /// Request with an explicit bearer token (or none).
    pub async fn req_as(
        &self,
        token: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut r = self.http.request(m, format!("{}{}", self.url, path));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
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

    /// Request as an external agent (MCP / CLI): authenticated, but never human-only.
    pub async fn req(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut r = self
            .http
            .request(m, format!("{}{}", self.url, path))
            .bearer_auth(&self.pat);
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

    /// Request as the signed-in human in the app — needed for human-only actions.
    pub async fn ui(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut r = self
            .http
            .request(m, format!("{}{}", self.url, path))
            .header("Origin", "http://localhost:1420")
            .bearer_auth(&self.session);
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

    /// A call that must succeed, made by the remote agent when it may (D-061), otherwise by the
    /// signed-in human (code, builds and deploys are never a remote agent's).
    pub async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let bare = path.split('?').next().unwrap_or(path);
        if method != "GET" && !kodehub_server::auth::remote_agent_may_write(bare) {
            return self.uok(method, path, body).await;
        }
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
