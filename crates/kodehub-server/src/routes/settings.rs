//! Settings: agent backend, models and the headless-agent token selector. The selector only
//! ever stores and returns the *name* of an environment variable, never its value.

use axum::extract::State;
use axum::Json;
use kodehub_core::config::Config;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

/// Names (never values) of env vars matching the pattern that are set and non-empty — asked of
/// the agent wrapper, which sources KODEHUB_ENV_FILE in its own process (decisions.md D-021).
pub fn available_token_vars(s: &AppState, pattern: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(e) = &s.engine {
        if let Ok(out) = std::process::Command::new(&e.wrapper)
            .args(["--list-names", pattern])
            .output()
        {
            names.extend(
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(String::from)
                    .filter(|l| !l.is_empty()),
            );
        }
    }
    if let Ok(re) = regex::Regex::new(pattern) {
        names.extend(std::env::vars_os().filter_map(|(k, v)| {
            let k = k.into_string().ok()?;
            (re.is_match(&k) && !v.is_empty()).then_some(k)
        }));
    }
    names.retain(|n| n != "ANTHROPIC_API_KEY");
    names.sort();
    names.dedup();
    names
}

fn view(s: &AppState, cfg: &Config) -> Value {
    json!({
        "agents": {
            "backend": cfg.agents.backend,
            "model": cfg.agents.model,
            "models": cfg.agents.models,
            "token_env": cfg.agents.token_env,
            "token_env_pattern": cfg.agents.token_env_pattern,
            "available_token_vars": available_token_vars(s, &cfg.agents.token_env_pattern),
            "api_key_configured": std::env::var_os("KODEHUB_ANTHROPIC_API_KEY").is_some_and(|v| !v.is_empty()),
            "recording": cfg.agents.recording,
        },
        "server": cfg.server,
    })
}

pub async fn get(State(s): State<AppState>) -> ApiResult<Value> {
    let cfg = s.config.read().await.clone();
    Ok(Json(view(&s, &cfg)))
}

pub async fn put(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(mut cfg): Json<Config>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let mut w = s.config.write().await;
    // the allowed token-name pattern is a local config-file setting, not API-editable
    cfg.agents.token_env_pattern = w.agents.token_env_pattern.clone();
    validate(&cfg)?;
    *w = cfg;
    w.save().map_err(|e| ApiError::internal(e.to_string()))?;
    let c = w.clone();
    drop(w);
    Ok(Json(view(&s, &c)))
}

/// Patch agent settings, e.g. `{"token_env": "DEV2_TOKEN"}` or `{"backend": "recorded"}`.
pub async fn patch_agents(
    State(s): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(p): Json<Value>,
) -> ApiResult<Value> {
    crate::require_ui(&s, &headers)?;
    let mut w = s.config.write().await;
    let mut cfg = w.clone();
    if let Some(v) = p.get("backend").and_then(Value::as_str) {
        cfg.agents.backend = v.into();
        if v == "recorded" {
            if let Some(e) = &s.engine {
                e.reset_recording();
            }
        }
    }
    if let Some(v) = p.get("model").and_then(Value::as_str) {
        cfg.agents.model = v.into();
    }
    if let Some(v) = p.get("token_env").and_then(Value::as_str) {
        cfg.agents.token_env = v.into();
    }
    if let Some(m) = p.get("models").and_then(Value::as_object) {
        for (k, v) in m {
            if let Some(v) = v.as_str() {
                cfg.agents.models.insert(k.clone(), v.into());
            }
        }
    }
    if let Some(v) = p.get("recording") {
        cfg.agents.recording = v.as_str().map(String::from);
    }
    validate(&cfg)?;
    *w = cfg;
    w.save().map_err(|e| ApiError::internal(e.to_string()))?;
    let c = w.clone();
    drop(w);
    s.events
        .emit("agent.activity", json!({"settings": "changed"}));
    Ok(Json(view(&s, &c)))
}

fn validate(cfg: &Config) -> Result<(), ApiError> {
    if !["claude_code", "api", "recorded"].contains(&cfg.agents.backend.as_str()) {
        return Err(ApiError::bad_request(
            "backend must be claude_code, api or recorded",
        ));
    }
    if !cfg.agents.token_env.is_empty() {
        if cfg.agents.token_env == "ANTHROPIC_API_KEY" {
            return Err(ApiError::bad_request(
                "ANTHROPIC_API_KEY may never be used by Kodehub",
            ));
        }
        let re = regex::Regex::new(&cfg.agents.token_env_pattern)
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        if !re.is_match(&cfg.agents.token_env) {
            return Err(ApiError::bad_request(format!(
                "token_env `{}` does not match the allowed pattern {}",
                cfg.agents.token_env, cfg.agents.token_env_pattern
            )));
        }
    }
    Ok(())
}
