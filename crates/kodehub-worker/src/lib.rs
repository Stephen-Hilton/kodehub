//! The deterministic container jobs (spec §5.3–5.4, §13). Each job reads its input from the
//! storage service, does one thing, writes its output back, and exits. No job calls an LLM or
//! writes the graph; the orchestrator records results.

pub mod git;

use anyhow::{Context, Result};
use kodehub_core::model::Language;
use kodehub_core::CodeGraph;
use kodehub_runner::Runner;
use kodehub_storage::{StorageClient, BUCKET};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Serialize, Deserialize)]
pub struct BuildInput {
    pub project: String,
    pub language: Language,
    pub tests: Vec<String>,
    pub graph: CodeGraph,
    pub artifact_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestInput {
    pub artifact_key: String,
    #[serde(default)]
    pub tests: Option<Vec<String>>,
}

pub async fn read_json<T: serde::de::DeserializeOwned>(s: &StorageClient, key: &str) -> Result<T> {
    let b = s
        .get(BUCKET, key)
        .await?
        .with_context(|| format!("job input {key} not found"))?;
    Ok(serde_json::from_slice(&b)?)
}

pub async fn write_json(s: &StorageClient, key: &str, v: &Value) -> Result<()> {
    s.put(BUCKET, key, serde_json::to_vec(v)?).await?;
    Ok(())
}

/// Build container: render + compile a test bundle once and publish it by its key.
pub async fn build(s: &StorageClient, runner: &Runner, input: &BuildInput) -> Result<Value> {
    if s.exists(BUCKET, &input.artifact_key).await? {
        return Ok(json!({"ok": true, "cached": true, "artifact_key": input.artifact_key}));
    }
    match runner
        .build_test_bundle(&input.graph, input.language, &input.tests)
        .await?
    {
        Ok((tar, manifest)) => {
            let size = tar.len();
            let etag = s.put(BUCKET, &input.artifact_key, tar).await?;
            Ok(
                json!({"ok": true, "cached": false, "artifact_key": input.artifact_key,
                      "manifest": manifest, "bytes": size, "etag": etag}),
            )
        }
        Err(log) => Ok(json!({"ok": false, "log": log})),
    }
}

/// Test container: fetch the bundle by key and run its shell scripts.
pub async fn test(s: &StorageClient, runner: &Runner, input: &TestInput) -> Result<Value> {
    let tar = s
        .get(BUCKET, &input.artifact_key)
        .await?
        .with_context(|| format!("artifact {} not found", input.artifact_key))?;
    let results = runner.run_test_bundle(&tar, input.tests.as_deref()).await?;
    Ok(json!({"ok": true, "results": results}))
}

/// Agent container: run one agent task on the chosen backend and exit (spec §4.2). The model
/// credential arrives in this container's environment only; the task's Kodehub token only
/// reaches the read-only graph tools.
pub async fn agent(runner: &Runner, input: &Value) -> Result<Value> {
    use kodehub_agents::AgentBackend;
    let task: kodehub_agents::AgentTask = serde_json::from_value(input["task"].clone())?;
    let server = input["server_url"]
        .as_str()
        .unwrap_or("http://kodehub-server:7878");
    let out = match input["backend"].as_str().unwrap_or("claude_code") {
        "recorded" => {
            let path = std::env::var("KODEHUB_RECORDING")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| "/app/recordings/demo-locked-account.json".into());
            kodehub_agents::RecordedBackend::from_file(&path)?
                .run(&task)
                .await?
        }
        "api" => {
            kodehub_agents::ApiBackend::from_env(server)?
                .run(&task)
                .await?
        }
        _ => {
            let wrapper = kodehub_agents::backend::claude_code::install_wrapper(&runner.home)?;
            kodehub_agents::ClaudeCodeBackend {
                wrapper,
                token_env: String::new(),
                kodehub_bin: "kodehub".into(),
                server_url: server.to_string(),
                workdir: runner.home.join("agent"),
                timeout: std::time::Duration::from_secs(1800),
                oauth_token: std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok(),
            }
            .run(&task)
            .await?
        }
    };
    Ok(json!({"ok": true, "output": out}))
}

/// Transcription container: run the local Whisper model over one audio file (spec §14.1).
/// `KODEHUB_TRANSCRIBE_CMD` is the command (the image sets `python3 /app/transcribe.py`); it gets
/// the audio file path as its last argument and prints the text.
pub async fn transcribe(s: &StorageClient, input: &Value) -> Result<Value> {
    let key = input["audio_key"]
        .as_str()
        .context("audio_key is required")?;
    let audio = s
        .get(BUCKET, key)
        .await?
        .with_context(|| format!("audio {key} not found"))?;
    let dir = tempfile::tempdir()?;
    let ext = input["ext"].as_str().unwrap_or("webm");
    let path = dir.path().join(format!("audio.{ext}"));
    std::fs::write(&path, &audio)?;
    let cmd = std::env::var("KODEHUB_TRANSCRIBE_CMD")
        .unwrap_or_else(|_| "python3 /app/transcribe.py".into());
    let mut parts = cmd.split_whitespace();
    let prog = parts.next().context("empty KODEHUB_TRANSCRIBE_CMD")?;
    let out = tokio::process::Command::new(prog)
        .args(parts)
        .arg(&path)
        .output()
        .await
        .with_context(|| format!("cannot run {prog}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "transcription failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // the audio is not kept once transcribed
    let _ = s.delete(BUCKET, key).await;
    Ok(json!({"ok": true, "text": String::from_utf8_lossy(&out.stdout).trim()}))
}
