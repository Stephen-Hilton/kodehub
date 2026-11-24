//! Kodehub storage service (spec §3.4): one S3-style object interface for the whole system,
//! over local disk, a cloud object store, or both (hybrid, with per-project backups to a cold
//! tier). ArangoDB holds metadata and hashes; the bytes live here.

pub mod backend;
pub mod backup;
pub mod client;
pub mod server;

pub use backend::{LocalBackend, ObjectInfo, Upstream, UpstreamAuth};
pub use client::{artifact_key, StorageClient, BUCKET};
pub use server::{Mode, Storage};

/// Build a storage service from `KODEHUB_STORAGE_*` environment variables.
pub fn from_env() -> anyhow::Result<Storage> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let mode = Mode::parse(&var("KODEHUB_STORAGE_MODE").unwrap_or_else(|| "local".into()))?;
    let local = var("KODEHUB_STORAGE_DIR").map(|d| LocalBackend { root: d.into() });
    let upstream = match var("KODEHUB_STORAGE_UPSTREAM_URL") {
        Some(url) => {
            let bucket =
                var("KODEHUB_STORAGE_UPSTREAM_BUCKET").unwrap_or_else(|| "kodehub-cold".into());
            let auth = match var("KODEHUB_STORAGE_UPSTREAM_AUTH")
                .as_deref()
                .unwrap_or("bearer")
            {
                "sigv4" => UpstreamAuth::SigV4 {
                    access_key: var("KODEHUB_STORAGE_UPSTREAM_ACCESS_KEY").unwrap_or_default(),
                    secret_key: var("KODEHUB_STORAGE_UPSTREAM_SECRET_KEY").unwrap_or_default(),
                    region: var("KODEHUB_STORAGE_UPSTREAM_REGION")
                        .unwrap_or_else(|| "us-east-1".into()),
                },
                _ => {
                    UpstreamAuth::Bearer(var("KODEHUB_STORAGE_UPSTREAM_TOKEN").unwrap_or_default())
                }
            };
            Some(Upstream::new(&url, &bucket, auth)?)
        }
        None => None,
    };
    Storage::new(mode, local, upstream, var("KODEHUB_STORAGE_TOKEN"))
}
