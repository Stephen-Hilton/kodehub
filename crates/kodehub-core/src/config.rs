//! `$KODEHUB_HOME/config.toml` (default `~/.kode`). The Makefile points KODEHUB_HOME at
//! `<repo>/.kode` so a pilot checkout keeps everything inside the repo.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub agents: AgentsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsConfig {
    /// `claude_code` (default), `api` or `recorded`.
    pub backend: String,
    /// Default model for every agent.
    pub model: String,
    /// Per-agent model overrides (orchestrator, coder, reviewer, tester).
    pub models: BTreeMap<String, String>,
    /// Name (never the value) of the environment variable holding a Claude Code long-lived
    /// token for headless agents. Empty = use the `claude` CLI's own login.
    pub token_env: String,
    /// Regex for env var *names* offered in the token selector.
    pub token_env_pattern: String,
    /// Path of a recordings file for the `recorded` backend.
    pub recording: Option<String>,
    /// Max agent turns per task (Claude Code backend).
    pub max_turns: u32,
    /// Seconds before an agent task is abandoned.
    pub timeout_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind: "127.0.0.1:7878".into(),
            url: "http://127.0.0.1:7878".into(),
        }
    }
}

impl Default for AgentsConfig {
    fn default() -> Self {
        AgentsConfig {
            backend: "claude_code".into(),
            model: "claude-opus-5-5".into(),
            models: BTreeMap::new(),
            token_env: String::new(),
            token_env_pattern: "^DEV[0-9]+_TOKEN$".into(),
            recording: None,
            max_turns: 12,
            timeout_secs: 600,
        }
    }
}

pub fn kodehub_home() -> PathBuf {
    if let Ok(h) = std::env::var("KODEHUB_HOME") {
        return PathBuf::from(h);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kode")
}

impl Config {
    pub fn path() -> PathBuf {
        kodehub_home().join("config.toml")
    }

    pub fn load() -> Config {
        match std::fs::read_to_string(Self::path()) {
            Err(_) => Config::default(),
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                eprintln!(
                    "warning: {} is invalid ({e}); using defaults",
                    Self::path().display()
                );
                Config::default()
            }),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = Self::path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, toml::to_string_pretty(self).expect("config serialises"))
    }

    pub fn model_for(&self, role: &str) -> String {
        self.agents
            .models
            .get(role)
            .cloned()
            .unwrap_or_else(|| self.agents.model.clone())
    }
}
