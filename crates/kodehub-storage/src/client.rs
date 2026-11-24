//! Client for the storage service (used by the orchestrator, build and test containers).

use anyhow::{bail, Result};
use bytes::Bytes;

use crate::backend::{parse_list_xml, ObjectInfo};

#[derive(Debug, Clone)]
pub struct StorageClient {
    pub base: String,
    pub token: Option<String>,
    http: reqwest::Client,
}

/// The bucket Kodehub keeps project data in.
pub const BUCKET: &str = "kodehub";

/// Object key of a build artifact: `projects/<project>/artifacts/<closure>/<target>.tar`.
pub fn artifact_key(project: &str, closure_hash: &str, target: &str) -> String {
    format!("projects/{project}/artifacts/{closure_hash}/{target}.tar")
}

impl StorageClient {
    pub fn new(base: &str, token: Option<String>) -> Self {
        StorageClient {
            base: base.trim_end_matches('/').to_string(),
            token: token.filter(|t| !t.is_empty()),
            http: reqwest::Client::new(),
        }
    }

    /// `KODEHUB_STORAGE_URL` / `KODEHUB_STORAGE_TOKEN`.
    pub fn from_env() -> Option<Self> {
        let base = std::env::var("KODEHUB_STORAGE_URL")
            .ok()
            .filter(|s| !s.is_empty())?;
        Some(Self::new(
            &base,
            std::env::var("KODEHUB_STORAGE_TOKEN").ok(),
        ))
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let r = self.http.request(m, format!("{}/{}", self.base, path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    pub async fn put(&self, bucket: &str, key: &str, data: impl Into<Bytes>) -> Result<String> {
        let r = self
            .req(reqwest::Method::PUT, &format!("{bucket}/{key}"))
            .body(data.into())
            .send()
            .await?;
        if !r.status().is_success() {
            bail!(
                "storage PUT {key}: {} {}",
                r.status(),
                r.text().await.unwrap_or_default()
            );
        }
        Ok(r.headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim_matches('"')
            .to_string())
    }

    pub async fn get(&self, bucket: &str, key: &str) -> Result<Option<Bytes>> {
        let r = self
            .req(reqwest::Method::GET, &format!("{bucket}/{key}"))
            .send()
            .await?;
        if r.status().as_u16() == 404 {
            return Ok(None);
        }
        if !r.status().is_success() {
            bail!("storage GET {key}: {}", r.status());
        }
        Ok(Some(r.bytes().await?))
    }

    pub async fn exists(&self, bucket: &str, key: &str) -> Result<bool> {
        let r = self
            .req(reqwest::Method::HEAD, &format!("{bucket}/{key}"))
            .send()
            .await?;
        Ok(r.status().is_success())
    }

    pub async fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        let r = self
            .req(reqwest::Method::DELETE, &format!("{bucket}/{key}"))
            .send()
            .await?;
        if !r.status().is_success() && r.status().as_u16() != 404 {
            bail!("storage DELETE {key}: {}", r.status());
        }
        Ok(())
    }

    pub async fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<ObjectInfo>> {
        let r = self
            .req(reqwest::Method::GET, bucket)
            .query(&[("list-type", "2"), ("prefix", prefix)])
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("storage LIST {prefix}: {}", r.status());
        }
        Ok(parse_list_xml(&r.text().await?))
    }

    pub async fn admin(
        &self,
        method: reqwest::Method,
        path: &str,
        project: Option<&str>,
    ) -> Result<serde_json::Value> {
        let mut r = self.req(method, path);
        if let Some(p) = project {
            r = r.query(&[("project", p)]);
        }
        let r = r.send().await?;
        let ok = r.status().is_success();
        let v: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
        if !ok {
            bail!("storage {path}: {v}");
        }
        Ok(v)
    }
}
