//! `kodehub-storage`: the storage service container.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let s = kodehub_storage::from_env()?;
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
