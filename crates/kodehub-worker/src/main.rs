//! `kodehub-worker <build|test|git-import|git-export> --input <key> --output <key>`

use clap::Parser;
use kodehub_runner::Runner;
use kodehub_storage::StorageClient;
use serde_json::json;

#[derive(Parser)]
#[command(name = "kodehub-worker", about = "Kodehub deterministic container job")]
struct Args {
    /// build | test | git-import | git-export
    kind: String,
    #[arg(long)]
    input: String,
    #[arg(long)]
    output: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let a = Args::parse();
    let runner = Runner::from_env();
    std::fs::create_dir_all(&runner.home)?;
    let s = StorageClient::from_env_or_local(&runner.home);
    let out = match a.kind.as_str() {
        "build" => {
            let input = kodehub_worker::read_json(&s, &a.input).await?;
            kodehub_worker::build(&s, &runner, &input).await
        }
        "test" => {
            let input = kodehub_worker::read_json(&s, &a.input).await?;
            kodehub_worker::test(&s, &runner, &input).await
        }
        "agent" => {
            let input: serde_json::Value = kodehub_worker::read_json(&s, &a.input).await?;
            kodehub_worker::agent(&runner, &input).await
        }
        "git-import" => {
            let input = kodehub_worker::read_json(&s, &a.input).await?;
            kodehub_worker::git::import(&input).await
        }
        "git-export" => {
            let input = kodehub_worker::read_json(&s, &a.input).await?;
            kodehub_worker::git::export(&input).await
        }
        other => Err(anyhow::anyhow!("unknown job kind `{other}`")),
    };
    let v = match out {
        Ok(v) => v,
        Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
    };
    kodehub_worker::write_json(&s, &a.output, &v).await?;
    eprintln!("{} job done: ok={}", a.kind, v["ok"]);
    if v["ok"] != true && v.get("error").is_some() {
        std::process::exit(1);
    }
    Ok(())
}
