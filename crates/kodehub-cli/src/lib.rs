//! `kodehub` CLI and MCP server. Both talk to the server only through its HTTP API.

pub mod mcp;

use serde_json::Value;

#[derive(Clone)]
pub struct Client {
    pub base: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: &str) -> Self {
        Client {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> anyhow::Result<Value> {
        let m = reqwest::Method::from_bytes(method.as_bytes())?;
        let mut req = self.http.request(m, format!("{}{}", self.base, path));
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("cannot reach kodehub-server at {}: {e}", self.base))?;
        let status = resp.status();
        let text = resp.text().await?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if !status.is_success() {
            let msg = v
                .get("error")
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| v.to_string());
            anyhow::bail!("{status}: {msg}");
        }
        Ok(v)
    }

    /// Invoke a catalog tool by name.
    pub async fn tool(&self, name: &str, args: &Value) -> anyhow::Result<Value> {
        let defs = kodehub_core::tools::catalog();
        let def = defs
            .iter()
            .find(|d| d.name == name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool {name}"))?;
        let (m, p, b) = kodehub_core::tools::to_request(def, args);
        self.call(&m, &p, b.as_ref()).await
    }
}

pub fn server_url(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("KODEHUB_SERVER_URL").ok())
        .unwrap_or_else(|| kodehub_core::config::Config::load().server.url)
}
