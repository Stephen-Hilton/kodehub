//! Build cache, compile, test, run-now and sandbox (doc 03).

pub mod build;
pub mod exec;
pub mod pyenv;
pub mod run;
pub mod testing;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kodehub_render::RenderError;
use kodehub_store::StoreError;

pub use build::{BuildOutcome, BuildTarget};
pub use exec::{ExecOutput, OutputLine};
pub use run::{RunRecord, RunRequest};
pub use testing::{TestOutcome, TestScope};

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Graph(#[from] kodehub_core::GraphError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("build failed:\n{0}")]
    Build(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, RunnerError>;

#[derive(Clone)]
pub struct Runner {
    pub home: PathBuf,
    /// Serialises cargo invocations that share CARGO_TARGET_DIR.
    build_lock: Arc<tokio::sync::Mutex<()>>,
    py_lock: Arc<tokio::sync::Mutex<()>>,
    pub run_timeout: Duration,
    pub test_timeout: Duration,
    /// Deny network to run-now processes where the OS allows (macOS sandbox-exec).
    pub sandbox: bool,
    /// Shared CARGO_TARGET_DIR (default `$KODEHUB_HOME/cargo-target`).
    pub cargo_target: PathBuf,
    /// Python virtualenv (default `$KODEHUB_HOME/pyenv`).
    pub pyenv: PathBuf,
}

impl Runner {
    pub fn new(home: PathBuf) -> Self {
        Runner {
            cargo_target: home.join("cargo-target"),
            pyenv: home.join("pyenv"),
            home,
            build_lock: Arc::new(tokio::sync::Mutex::new(())),
            py_lock: Arc::new(tokio::sync::Mutex::new(())),
            run_timeout: Duration::from_secs(10),
            test_timeout: Duration::from_secs(600),
            sandbox: true,
        }
    }

    /// From `KODEHUB_HOME`, with optional `KODEHUB_CARGO_TARGET` / `KODEHUB_PYENV` overrides
    /// (lets several homes share compiled dependencies and the Python venv).
    pub fn from_env() -> Self {
        let mut r = Runner::new(kodehub_core::config::kodehub_home());
        if let Ok(t) = std::env::var("KODEHUB_CARGO_TARGET") {
            r.cargo_target = t.into();
        }
        if let Ok(p) = std::env::var("KODEHUB_PYENV") {
            r.pyenv = p.into();
        }
        r
    }

    /// Cache location: closure hash + build target + a tag of the renderer version and the
    /// toolchain, so a renderer change or a rustc/python upgrade never reuses stale artifacts.
    pub fn cache_dir(&self, closure_hash: &str, target: &str) -> PathBuf {
        self.home
            .join("cache")
            .join(closure_hash)
            .join(format!("{target}-{}", self.toolchain_tag(target)))
    }

    pub fn toolchain_tag(&self, target: &str) -> String {
        static RUST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        if let Some(base) = target.split_once("+ext").map(|x| x.0) {
            // python artifact embedding a compiled Rust extension: both toolchains matter
            let a = self.toolchain_tag(base);
            let b = self.toolchain_tag("rust-run");
            return blake3::hash(format!("{a}{b}").as_bytes()).to_hex()[..8].to_string();
        }
        let tool = if target.starts_with("rust") {
            RUST.get_or_init(|| {
                std::process::Command::new(build::cargo_bin().with_file_name("rustc"))
                    .arg("-V")
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default()
            })
            .clone()
        } else {
            std::process::Command::new(self.python())
                .arg("--version")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        };
        blake3::hash(format!("{}|{tool}", kodehub_render::GENERATOR).as_bytes()).to_hex()[..8]
            .to_string()
    }

    pub fn cargo_target_dir(&self) -> PathBuf {
        self.cargo_target.clone()
    }

    pub fn render_root(&self) -> PathBuf {
        self.home.join("render")
    }
}
