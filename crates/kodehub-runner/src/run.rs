//! Run now (doc 03): cached build → harness → JSON in, JSON out, 10 s timeout, no network.

use std::time::Instant;

use kodehub_core::model::{now_iso, Language};
use kodehub_core::CodeGraph;
use kodehub_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::build::BuildTarget;
use crate::exec::{self, OutputLine};
use crate::{Result, Runner};

pub const RESULT_MARKER: &str = "\u{1e}KODEHUB_RESULT ";

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub symbol: String,
    pub args: Value,
    pub environment: String,
    /// Release the code came from (non-dev environments).
    pub release: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    #[serde(rename = "_key")]
    pub key: String,
    pub symbol: String,
    pub display_name: String,
    pub version: String,
    pub closure_hash: String,
    pub environment: String,
    pub release: Option<String>,
    pub args: Value,
    pub result: Value,
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cache_hit: bool,
    pub build_ms: u64,
    pub exec_ms: u64,
    pub duration_ms: u64,
    pub ran_at: String,
}

impl Runner {
    /// Execute a function with JSON args. Compiles only on a cache miss.
    pub async fn run_now(
        &self,
        g: &CodeGraph,
        req: &RunRequest,
        stream: Option<UnboundedSender<OutputLine>>,
    ) -> Result<RunRecord> {
        let start = Instant::now();
        kodehub_render::runnable(g, &req.symbol)?;
        let sym = g.symbol(&req.symbol)?.clone();
        let target = BuildTarget::Run {
            language: sym.language,
            symbol: req.symbol.clone(),
        };
        let b = self.build(g, &target).await?;
        let base = RunRecord {
            key: format!("run-{}", uuid_like()),
            symbol: req.symbol.clone(),
            display_name: sym.display_name.clone(),
            version: sym.version.clone(),
            closure_hash: b.closure_hash.clone(),
            environment: req.environment.clone(),
            release: req.release.clone(),
            args: req.args.clone(),
            result: Value::Null,
            ok: false,
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
            timed_out: false,
            cache_hit: b.cache_hit,
            build_ms: b.duration_ms,
            exec_ms: 0,
            duration_ms: 0,
            ran_at: now_iso(),
        };
        if !b.ok {
            return Ok(RunRecord {
                stderr: format!("build failed:\n{}", b.log),
                duration_ms: start.elapsed().as_millis() as u64,
                ..base
            });
        }
        let input = serde_json::to_string(&req.args).unwrap_or_else(|_| "{}".into());
        let cmd = match sym.language {
            Language::Rust => {
                let mut c = exec::command(&b.dir.join("kodehub_run"), &[], self.sandbox);
                c.current_dir(&b.dir);
                c
            }
            Language::Python => {
                // The script's own directory is the artifact dir, so `kodehub_gen` resolves.
                let mut c = exec::command(&self.python(), &["kodehub_run.py"], self.sandbox);
                c.current_dir(&b.dir).env("PYTHONDONTWRITEBYTECODE", "1");
                c
            }
        };
        let out = exec::run(cmd, Some(input), self.run_timeout, stream).await?;
        let mut result = Value::Null;
        let mut stdout_lines = Vec::new();
        for line in out.stdout.lines() {
            if let Some(r) = line.strip_prefix(RESULT_MARKER) {
                result = serde_json::from_str(r).unwrap_or(Value::String(r.into()));
            } else {
                stdout_lines.push(line);
            }
        }
        Ok(RunRecord {
            ok: out.success(),
            result,
            stdout: stdout_lines.join("\n"),
            stderr: out.stderr.clone(),
            exit_code: out.exit_code,
            timed_out: out.timed_out,
            exec_ms: out.duration_ms,
            duration_ms: start.elapsed().as_millis() as u64,
            ..base
        })
    }

    /// Store a run in `runs`.
    pub async fn record_run(&self, store: &Store, r: &RunRecord) -> Result<Value> {
        let doc = serde_json::to_value(r).unwrap_or(json!({}));
        Ok(store.db.insert("runs", &doc, None).await?)
    }
}

pub(crate) fn uuid_like() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    blake3::hash(format!("{t}-{:?}", std::thread::current().id()).as_bytes()).to_hex()[..16]
        .to_string()
}
