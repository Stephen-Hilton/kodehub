//! `kodehub-storage`: the storage service container.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let s = kodehub_storage::from_env()?;
    // an S3 upstream (SeaweedFS, AWS S3, …) may start after us or be brand new: create the
    // bucket, retrying for a while; serving local objects does not wait on it
    if let Some(up) = s.upstream.clone() {
        tokio::spawn(async move {
            for attempt in 1..=60 {
                match up.ensure_bucket().await {
                    Ok(()) => {
                        tracing::info!("upstream bucket `{}` ready", up.bucket);
                        return;
                    }
                    Err(e) if attempt == 60 => tracing::warn!("upstream bucket: {e}"),
                    Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
                }
            }
        });
    }
    let hours: f64 = std::env::var("KODEHUB_STORAGE_BACKUP_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6.0);
    kodehub_storage::server::spawn_backups(s.clone(), hours);
    let bind = std::env::var("KODEHUB_STORAGE_BIND").unwrap_or_else(|_| "0.0.0.0:9000".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("kodehub-storage ({}) listening on {bind}", s.mode.as_str());
    axum::serve(listener, kodehub_storage::server::router(s)).await?;
    Ok(())
}
