//! The storage service's HTTP interface: S3 path-style object operations
//! (`PUT/GET/HEAD/DELETE /<bucket>/<key>`, `GET /<bucket>?list-type=2&prefix=`), plus backup
//! administration (`POST /_backup`, `POST /_restore`, `GET /_backups`). Requests carry a bearer
//! token when one is configured; `/health` is public.

use std::sync::Arc;

use anyhow::Result;
use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::backend::{list_xml, valid_bucket, valid_key, LocalBackend, Upstream};
use crate::backup;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Everything on the cluster's own disks.
    Local,
    /// Straight through to the upstream object store.
    Cloud,
    /// Local, with scheduled per-project backups to the upstream cold tier.
    Hybrid,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode> {
        match s {
            "local" => Ok(Mode::Local),
            "cloud" => Ok(Mode::Cloud),
            "hybrid" => Ok(Mode::Hybrid),
            o => anyhow::bail!("storage mode must be local, cloud or hybrid (got `{o}`)"),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Local => "local",
            Mode::Cloud => "cloud",
            Mode::Hybrid => "hybrid",
        }
    }
}

#[derive(Clone)]
pub struct Storage {
    pub mode: Mode,
    pub local: Option<LocalBackend>,
    pub upstream: Option<Upstream>,
    pub token: Option<Arc<String>>,
}

impl Storage {
    pub fn new(
        mode: Mode,
        local: Option<LocalBackend>,
        upstream: Option<Upstream>,
        token: Option<String>,
    ) -> Result<Self> {
        match mode {
            Mode::Local if local.is_none() => anyhow::bail!("local mode needs a directory"),
            Mode::Cloud if upstream.is_none() => anyhow::bail!("cloud mode needs an upstream"),
            Mode::Hybrid if local.is_none() || upstream.is_none() => {
                anyhow::bail!("hybrid mode needs a directory and an upstream")
            }
            _ => {}
        }
        Ok(Storage {
            mode,
            local,
            upstream,
            token: token.filter(|t| !t.is_empty()).map(Arc::new),
        })
    }

    /// In cloud mode a client bucket becomes a key prefix in the upstream bucket.
    fn up_key(bucket: &str, key: &str) -> String {
        format!("{bucket}/{key}")
    }

    pub async fn put(&self, bucket: &str, key: &str, data: Bytes) -> Result<String> {
        match self.mode {
            Mode::Cloud => {
                self.upstream
                    .as_ref()
                    .unwrap()
                    .put(&Self::up_key(bucket, key), data)
                    .await
            }
            _ => self.local.as_ref().unwrap().put(bucket, key, data).await,
        }
    }

    pub async fn get(&self, bucket: &str, key: &str) -> Result<Option<Bytes>> {
        match self.mode {
            Mode::Cloud => {
                self.upstream
                    .as_ref()
                    .unwrap()
                    .get(&Self::up_key(bucket, key))
                    .await
            }
            _ => self.local.as_ref().unwrap().get(bucket, key).await,
        }
    }

    pub async fn delete(&self, bucket: &str, key: &str) -> Result<bool> {
        match self.mode {
            Mode::Cloud => {
                self.upstream
                    .as_ref()
                    .unwrap()
                    .delete(&Self::up_key(bucket, key))
                    .await?;
                Ok(true)
            }
            _ => self.local.as_ref().unwrap().delete(bucket, key).await,
        }
    }

    pub async fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<crate::ObjectInfo>> {
        match self.mode {
            Mode::Cloud => {
                let p = format!("{bucket}/");
                Ok(self
                    .upstream
                    .as_ref()
                    .unwrap()
                    .list(&format!("{p}{prefix}"))
                    .await?
                    .into_iter()
                    .map(|mut o| {
                        o.key = o.key.strip_prefix(&p).unwrap_or(&o.key).to_string();
                        o
                    })
                    .collect())
            }
            _ => self.local.as_ref().unwrap().list(bucket, prefix).await,
        }
    }

    /// Back up every project (hybrid mode).
    pub async fn backup_all(&self) -> Result<Vec<backup::BackupReport>> {
        let (Some(l), Some(u)) = (&self.local, &self.upstream) else {
            anyhow::bail!("backups need a local tier and an upstream (hybrid mode)");
        };
        let mut out = Vec::new();
        for p in backup::projects(l).await? {
            out.push(backup::backup_project(l, u, &p).await?);
        }
        Ok(out)
    }
}

fn err(status: StatusCode, msg: impl ToString) -> Response {
    (status, Json(json!({"error": msg.to_string()}))).into_response()
}

async fn auth(State(s): State<Storage>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    if let Some(t) = &s.token {
        let ok = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|v| constant_eq(v.trim().as_bytes(), t.as_bytes()));
        if !ok {
            return err(StatusCode::UNAUTHORIZED, "storage token required");
        }
    }
    next.run(req).await
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn health(State(s): State<Storage>) -> Json<serde_json::Value> {
    Json(json!({"status": "ok", "mode": s.mode.as_str(),
                "upstream": s.upstream.as_ref().map(|u| u.endpoint.to_string())}))
}

#[derive(Deserialize)]
struct ListQ {
    #[serde(default)]
    prefix: String,
}

async fn list(
    State(s): State<Storage>,
    Path(bucket): Path<String>,
    Query(q): Query<ListQ>,
) -> Response {
    if !valid_bucket(&bucket) {
        return err(StatusCode::BAD_REQUEST, "invalid bucket");
    }
    match s.list(&bucket, &q.prefix).await {
        Ok(items) => (
            [(axum::http::header::CONTENT_TYPE, "application/xml")],
            list_xml(&bucket, &q.prefix, &items),
        )
            .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

async fn object(
    State(s): State<Storage>,
    method: Method,
    Path((bucket, key)): Path<(String, String)>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !valid_bucket(&bucket) || !valid_key(&key) {
        return err(StatusCode::BAD_REQUEST, "invalid bucket or key");
    }
    let res: Result<Response> = async {
        Ok(match method {
            Method::PUT => {
                let tag = s.put(&bucket, &key, body).await?;
                let mut r = StatusCode::OK.into_response();
                r.headers_mut()
                    .insert("etag", HeaderValue::from_str(&format!("\"{tag}\""))?);
                r
            }
            Method::GET | Method::HEAD => match s.get(&bucket, &key).await? {
                Some(b) => {
                    let tag = crate::backend::etag(&b);
                    let len = b.len();
                    let mut r = if method == Method::HEAD {
                        StatusCode::OK.into_response()
                    } else {
                        b.into_response()
                    };
                    r.headers_mut()
                        .insert("etag", HeaderValue::from_str(&format!("\"{tag}\""))?);
                    r.headers_mut()
                        .insert("x-object-size", HeaderValue::from_str(&len.to_string())?);
                    r
                }
                None => err(StatusCode::NOT_FOUND, "no such key"),
            },
            Method::DELETE => {
                s.delete(&bucket, &key).await?;
                StatusCode::NO_CONTENT.into_response()
            }
            _ => err(StatusCode::METHOD_NOT_ALLOWED, "unsupported method"),
        })
    }
    .await;
    res.unwrap_or_else(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

#[derive(Deserialize)]
struct ProjectQ {
    project: Option<String>,
}

async fn run_backup(State(s): State<Storage>, Query(q): Query<ProjectQ>) -> Response {
    let res = match q.project {
        Some(p) => match (&s.local, &s.upstream) {
            (Some(l), Some(u)) => backup::backup_project(l, u, &p).await.map(|r| vec![r]),
            _ => Err(anyhow::anyhow!("backups need hybrid mode")),
        },
        None => s.backup_all().await,
    };
    match res {
        Ok(r) => Json(json!({"backups": r})).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

async fn run_restore(State(s): State<Storage>, Query(q): Query<ProjectQ>) -> Response {
    let Some(p) = q.project else {
        return err(
            StatusCode::BAD_REQUEST,
            "restore one project at a time: ?project=",
        );
    };
    let (Some(l), Some(u)) = (&s.local, &s.upstream) else {
        return err(StatusCode::BAD_REQUEST, "restores need hybrid mode");
    };
    match backup::restore_project(l, u, &p).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, e),
    }
}

async fn list_backups(State(s): State<Storage>, Query(q): Query<ProjectQ>) -> Response {
    let Some(u) = &s.upstream else {
        return err(StatusCode::BAD_REQUEST, "no upstream configured");
    };
    let Some(p) = q.project else {
        return err(StatusCode::BAD_REQUEST, "?project= is required");
    };
    match backup::bundles(u, &p).await {
        Ok(v) => Json(json!({"project": p, "bundles": v})).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

pub fn router(s: Storage) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/_backup", post(run_backup))
        .route("/_restore", post(run_restore))
        .route("/_backups", get(list_backups))
        .route("/{bucket}", get(list))
        .route(
            "/{bucket}/{*key}",
            get(object).put(object).head(object).delete(object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(s.clone(), auth))
        .with_state(s)
}

/// Run scheduled backups every `hours` (hybrid mode).
pub fn spawn_backups(s: Storage, hours: f64) {
    if s.mode != Mode::Hybrid || hours <= 0.0 {
        return;
    }
    tokio::spawn(async move {
        let every = std::time::Duration::from_secs_f64(hours * 3600.0);
        loop {
            tokio::time::sleep(every).await;
            match s.backup_all().await {
                Ok(r) => tracing::info!("backup: {} project(s)", r.len()),
                Err(e) => tracing::warn!("backup failed: {e}"),
            }
        }
    });
}
