//! Object backends: a local directory, and an upstream object store (real S3 via SigV4
//! presigned requests, or another kodehub-storage via bearer token).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use serde::{Deserialize, Serialize};

/// Object listing entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
    pub etag: String,
}

pub fn etag(data: &[u8]) -> String {
    blake3::hash(data).to_hex()[..32].to_string()
}

/// Keys are `/`-separated; no empty, `.` or `..` segments, no backslashes or NULs.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 1024
        && !key.contains('\\')
        && !key.contains('\0')
        && key
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

pub fn valid_bucket(b: &str) -> bool {
    (3..=63).contains(&b.len())
        && b.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !b.starts_with('-')
}

/// Objects as files under `root/<bucket>/<key>`.
#[derive(Debug, Clone)]
pub struct LocalBackend {
    pub root: PathBuf,
}

impl LocalBackend {
    fn path(&self, bucket: &str, key: &str) -> Result<PathBuf> {
        if !valid_bucket(bucket) || !valid_key(key) {
            bail!("invalid bucket or key");
        }
        Ok(self.root.join(bucket).join(key))
    }

    pub async fn put(&self, bucket: &str, key: &str, data: Bytes) -> Result<String> {
        let p = self.path(bucket, key)?;
        if let Some(d) = p.parent() {
            tokio::fs::create_dir_all(d).await?;
        }
        // write-then-rename so readers never see a partial object
        let tmp = p.with_extension(format!("tmp-{}", std::process::id()));
        tokio::fs::write(&tmp, &data).await?;
        tokio::fs::rename(&tmp, &p).await?;
        Ok(etag(&data))
    }

    pub async fn get(&self, bucket: &str, key: &str) -> Result<Option<Bytes>> {
        let p = self.path(bucket, key)?;
        match tokio::fs::read(&p).await {
            Ok(b) => Ok(Some(Bytes::from(b))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn delete(&self, bucket: &str, key: &str) -> Result<bool> {
        let p = self.path(bucket, key)?;
        match tokio::fs::remove_file(&p).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<ObjectInfo>> {
        if !valid_bucket(bucket) {
            bail!("invalid bucket");
        }
        let base = self.root.join(bucket);
        let mut out = Vec::new();
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            let mut rd = match tokio::fs::read_dir(&dir).await {
                Ok(r) => r,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            while let Some(ent) = rd.next_entry().await? {
                let p = ent.path();
                let ft = ent.file_type().await?;
                if ft.is_dir() {
                    stack.push(p);
                } else if ft.is_file() {
                    let rel = rel_key(&base, &p)?;
                    if rel.contains(".tmp-") || !rel.starts_with(prefix) {
                        continue;
                    }
                    let data = tokio::fs::read(&p).await?;
                    out.push(ObjectInfo {
                        key: rel,
                        size: data.len() as u64,
                        etag: etag(&data),
                    });
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }
}

fn rel_key(base: &Path, p: &Path) -> Result<String> {
    let rel = p.strip_prefix(base).context("outside bucket")?;
    Ok(rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/"))
}

/// How the upstream authenticates requests.
#[derive(Debug, Clone)]
pub enum UpstreamAuth {
    /// Another kodehub-storage (or any server taking a bearer token).
    Bearer(String),
    /// Real S3 / S3-compatible storage: SigV4 presigned requests.
    SigV4 {
        access_key: String,
        secret_key: String,
        region: String,
    },
}

/// An upstream S3-style object store (the cloud / cold tier).
#[derive(Debug, Clone)]
pub struct Upstream {
    pub endpoint: url::Url,
    pub bucket: String,
    pub auth: UpstreamAuth,
    http: reqwest::Client,
}

const PRESIGN: Duration = Duration::from_secs(300);

impl Upstream {
    pub fn new(endpoint: &str, bucket: &str, auth: UpstreamAuth) -> Result<Self> {
        Ok(Upstream {
            endpoint: url::Url::parse(endpoint).context("upstream endpoint")?,
            bucket: bucket.to_string(),
            auth,
            http: reqwest::Client::new(),
        })
    }

    fn s3(&self) -> Result<(Bucket, Credentials)> {
        let UpstreamAuth::SigV4 {
            access_key,
            secret_key,
            region,
        } = &self.auth
        else {
            bail!("not a SigV4 upstream");
        };
        let b = Bucket::new(
            self.endpoint.clone(),
            UrlStyle::Path,
            self.bucket.clone(),
            region.clone(),
        )
        .map_err(|e| anyhow!("bucket: {e}"))?;
        Ok((b, Credentials::new(access_key.clone(), secret_key.clone())))
    }

    fn plain_url(&self, key: &str) -> String {
        format!(
            "{}/{}/{}",
            self.endpoint.as_str().trim_end_matches('/'),
            self.bucket,
            key
        )
    }

    /// The request URL for an object operation (presigned for SigV4).
    pub fn object_url(&self, method: &str, key: &str) -> Result<url::Url> {
        match &self.auth {
            UpstreamAuth::Bearer(_) => Ok(url::Url::parse(&self.plain_url(key))?),
            UpstreamAuth::SigV4 { .. } => {
                let (b, c) = self.s3()?;
                Ok(match method {
                    "PUT" => b.put_object(Some(&c), key).sign(PRESIGN),
                    "GET" => b.get_object(Some(&c), key).sign(PRESIGN),
                    "DELETE" => b.delete_object(Some(&c), key).sign(PRESIGN),
                    other => bail!("unsupported method {other}"),
                })
            }
        }
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            UpstreamAuth::Bearer(t) => rb.bearer_auth(t),
            UpstreamAuth::SigV4 { .. } => rb,
        }
    }

    pub async fn put(&self, key: &str, data: Bytes) -> Result<String> {
        let tag = etag(&data);
        let r = self
            .authed(self.http.put(self.object_url("PUT", key)?))
            .body(data)
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("upstream PUT {key}: {}", r.status());
        }
        Ok(tag)
    }

    pub async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let r = self
            .authed(self.http.get(self.object_url("GET", key)?))
            .send()
            .await?;
        if r.status().as_u16() == 404 {
            return Ok(None);
        }
        if !r.status().is_success() {
            bail!("upstream GET {key}: {}", r.status());
        }
        Ok(Some(r.bytes().await?))
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        let r = self
            .authed(self.http.delete(self.object_url("DELETE", key)?))
            .send()
            .await?;
        if !r.status().is_success() && r.status().as_u16() != 404 {
            bail!("upstream DELETE {key}: {}", r.status());
        }
        Ok(())
    }

    /// Create the upstream bucket if it does not exist yet (S3 CreateBucket; "already exists"
    /// counts as success). A bearer upstream is another kodehub-storage, whose buckets are
    /// implicit, so this is a no-op there.
    pub async fn ensure_bucket(&self) -> Result<()> {
        if let UpstreamAuth::Bearer(_) = self.auth {
            return Ok(());
        }
        let (b, c) = self.s3()?;
        let r = self
            .http
            .put(b.create_bucket(&c).sign(PRESIGN))
            .send()
            .await?;
        let status = r.status();
        if status.is_success() {
            return Ok(());
        }
        let body = r.text().await.unwrap_or_default();
        if status.as_u16() == 409
            && (body.contains("BucketAlreadyOwnedByYou") || body.contains("BucketAlreadyExists"))
        {
            return Ok(());
        }
        bail!("upstream CreateBucket {}: {status} {body}", self.bucket)
    }

    /// ListObjectsV2 (keys and sizes) under a prefix.
    pub async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>> {
        let url = match &self.auth {
            UpstreamAuth::Bearer(_) => {
                let mut u = url::Url::parse(&format!(
                    "{}/{}",
                    self.endpoint.as_str().trim_end_matches('/'),
                    self.bucket
                ))?;
                u.query_pairs_mut()
                    .append_pair("list-type", "2")
                    .append_pair("prefix", prefix);
                u
            }
            UpstreamAuth::SigV4 { .. } => {
                let (b, c) = self.s3()?;
                let mut a = b.list_objects_v2(Some(&c));
                a.with_prefix(prefix);
                a.sign(PRESIGN)
            }
        };
        let r = self.authed(self.http.get(url)).send().await?;
        if !r.status().is_success() {
            bail!("upstream LIST {prefix}: {}", r.status());
        }
        let xml = r.text().await?;
        Ok(parse_list_xml(&xml))
    }
}

/// Minimal ListBucketResult parser: `<Key>`, `<Size>`, `<ETag>` per `<Contents>`.
pub fn parse_list_xml(xml: &str) -> Vec<ObjectInfo> {
    let tag = |s: &str, t: &str| -> Option<String> {
        let a = s.find(&format!("<{t}>"))? + t.len() + 2;
        let b = s[a..].find(&format!("</{t}>"))? + a;
        Some(xml_unescape(&s[a..b]))
    };
    xml.split("<Contents>")
        .skip(1)
        .filter_map(|c| {
            let c = c.split("</Contents>").next()?;
            Some(ObjectInfo {
                key: tag(c, "Key")?,
                size: tag(c, "Size").and_then(|s| s.parse().ok()).unwrap_or(0),
                etag: tag(c, "ETag")
                    .map(|e| e.trim_matches('"').to_string())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// S3 ListObjectsV2 response body.
pub fn list_xml(bucket: &str, prefix: &str, items: &[ObjectInfo]) -> String {
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>{}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><IsTruncated>false</IsTruncated>",
        xml_escape(bucket),
        xml_escape(prefix),
        items.len()
    );
    for i in items {
        s.push_str(&format!(
            "<Contents><Key>{}</Key><Size>{}</Size><ETag>\"{}\"</ETag></Contents>",
            xml_escape(&i.key),
            i.size,
            i.etag
        ));
    }
    s.push_str("</ListBucketResult>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert!(valid_key("projects/a/artifacts/x.tar"));
        assert!(!valid_key("../etc/passwd"));
        assert!(!valid_key("a//b"));
        assert!(!valid_key("a/./b"));
        assert!(valid_bucket("kodehub"));
        assert!(!valid_bucket("Kodehub"));
    }

    #[test]
    fn list_roundtrip() {
        let items = vec![ObjectInfo {
            key: "a/b&c".into(),
            size: 3,
            etag: "e1".into(),
        }];
        assert_eq!(parse_list_xml(&list_xml("kodehub", "a/", &items)), items);
    }

    #[test]
    fn sigv4_urls_are_presigned() {
        let u = Upstream::new(
            "https://s3.eu-west-1.amazonaws.com",
            "cold",
            UpstreamAuth::SigV4 {
                access_key: "AKIDEXAMPLE".into(),
                secret_key: "secret".into(),
                region: "eu-west-1".into(),
            },
        )
        .unwrap();
        let url = u.object_url("PUT", "backups/p/000001-full.tar").unwrap();
        let q = url.query().unwrap_or("");
        assert!(q.contains("X-Amz-Signature=") && q.contains("X-Amz-Credential=AKIDEXAMPLE"));
        assert!(url.path().ends_with("/cold/backups/p/000001-full.tar"));
    }
}
