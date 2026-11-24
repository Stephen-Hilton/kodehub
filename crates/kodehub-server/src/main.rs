use clap::Parser;
use kodehub_core::config::Config;
use kodehub_runner::Runner;
use kodehub_store::{ArangoConfig, Store};

#[derive(Parser)]
#[command(name = "kodehub-server", about = "Kodehub graph server")]
struct Args {
    /// Address to bind (overrides config).
    #[arg(long, env = "KODEHUB_BIND")]
    bind: Option<String>,
    /// Serve the built desktop UI from this directory (e.g. app/dist).
    #[arg(long, env = "KODEHUB_STATIC_DIR")]
    static_dir: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();
    let args = Args::parse();
    let cfg = Config::load();
    let bind = args.bind.unwrap_or(cfg.server.bind.clone());
    let store = Store::open(ArangoConfig::from_env()).await?;
    let runner = Runner::from_env();
    std::fs::create_dir_all(&runner.home)?;
    let mut state = kodehub_server::AppState::new(store, runner, cfg);
    state.static_dir = args.static_dir.filter(|d| d.join("index.html").exists());
    let url = format!("http://{bind}");
    // the server's own origin (UI served from the server)
    let mut origins = (*state.origins).clone();
    if let Some(port) = bind.rsplit(':').next() {
        origins.push(format!("http://127.0.0.1:{port}"));
        origins.push(format!("http://localhost:{port}"));
    }
    state.origins = std::sync::Arc::new(origins);
    kodehub_server::install_agents(&mut state, &url, kodehub_agents::EngineOptions::default());
    // hand the per-launch UI secret to local UI processes (Vite dev, Tauri) via a 0600 file
    let token_file = state.runner.home.join("ui-token");
    std::fs::write(&token_file, state.ui_token.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600))?;
    }
    // the orchestrator's container work (test runs in build/test containers)
    if std::env::var("KODEHUB_WORKER").as_deref() != Ok("off") {
        let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "orchestrator".into());
        kodehub_server::orchestrator::Orchestrator {
            store: state.store.clone(),
            runner: state.runner.clone(),
            storage: kodehub_storage::StorageClient::from_env_or_local(&state.runner.home),
            jobs: kodehub_runner::jobs::JobRuntime::from_env(),
            owner: format!("{host}-{}", &uuid::Uuid::new_v4().simple().to_string()[..6]),
            events: state.events.clone(),
        }
        .spawn();
    }
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("kodehub-server listening on http://{bind}");
    kodehub_server::serve(listener, state).await?;
    Ok(())
}
