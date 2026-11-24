//! `kodehub-scheduler`: the scheduler container.

use axum::routing::get;
use kodehub_store::{ArangoConfig, Store};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let store = Store::open(ArangoConfig::from_env()).await?;
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "scheduler".into());
    let owner = format!("{host}-{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
    let tick: u64 = std::env::var("KODEHUB_SCHEDULER_TICK_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    // health endpoint for the kubelet
    let bind = std::env::var("KODEHUB_SCHEDULER_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tokio::spawn(async move {
        let app = axum::Router::new().route("/health", get(|| async { "ok" }));
        let _ = axum::serve(listener, app).await;
    });
    tracing::info!("kodehub-scheduler {owner}: tick every {tick}s");
    let mut s = kodehub_scheduler::Scheduler::new(store, owner);
    loop {
        match s.tick().await {
            Ok(0) => {}
            Ok(n) => tracing::info!("handed off {n} work item(s)"),
            Err(e) => tracing::warn!("tick failed: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(tick)).await;
    }
}
