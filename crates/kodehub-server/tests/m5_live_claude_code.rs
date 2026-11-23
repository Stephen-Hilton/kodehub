//! M5 acceptance (live): the doc 08 demo with real agents.
//! - Claude Code backend (default): only a developer's Claude login / long-lived token, no API key
//!   anywhere. Set KODEHUB_LIVE_DEMO=1 and KODEHUB_LIVE_TOKEN_ENV=<NAME> (e.g. DEV1_TOKEN).
//! - API backend: KODEHUB_LIVE_DEMO=1 KODEHUB_LIVE_BACKEND=api with KODEHUB_ANTHROPIC_API_KEY set.
mod common;

use std::time::Duration;

use common::*;
use kodehub_agents::EngineOptions;
use serde_json::{json, Value};

#[tokio::test]
async fn demo_scenario_on_claude_code_backend() {
    if std::env::var("KODEHUB_LIVE_DEMO").is_err() {
        eprintln!("skipped: set KODEHUB_LIVE_DEMO=1");
        return;
    }
    let backend = std::env::var("KODEHUB_LIVE_BACKEND").unwrap_or_else(|_| "claude_code".into());
    assert!(
        std::env::var_os("ANTHROPIC_API_KEY").is_none(),
        "ANTHROPIC_API_KEY may never be set for Kodehub"
    );
    let var = std::env::var("KODEHUB_LIVE_TOKEN_ENV").unwrap_or_default();
    if backend == "api" {
        assert!(
            std::env::var_os("KODEHUB_ANTHROPIC_API_KEY").is_some(),
            "API backend needs KODEHUB_ANTHROPIC_API_KEY"
        );
    } else if !var.is_empty() {
        assert!(std::env::var_os(&var).is_some(), "{var} not set");
    }
    let model = std::env::var("KODEHUB_LIVE_MODEL").unwrap_or_else(|_| "claude-opus-5-5".into());
    let t = TestServer::start_with("live", |st, url| {
        let mut cfg = st.config.try_write().unwrap();
        cfg.agents.backend = backend.clone();
        cfg.agents.token_env = var.clone();
        cfg.agents.model = model.clone();
        cfg.agents.timeout_secs = 900;
        drop(cfg);
        kodehub_server::install_agents(st, url, EngineOptions::default());
    })
    .await;
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    assert_eq!(
        t.ok("POST", "/api/tests/run", None).await["summary"]["failed"],
        0
    );

    t.uok(
        "POST",
        "/api/requirements/r1-generic-failure/edge_cases",
        Some(json!({
        "description": "A locked account must also return invalid_credentials",
        "input": {"username": "alice", "password": "correct horse", "locked": true},
        "expected": "invalid_credentials"})),
    )
    .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(3600);
    let mut approved = false;
    let cs = loop {
        assert!(std::time::Instant::now() < deadline, "live demo timed out");
        tokio::time::sleep(Duration::from_secs(3)).await;
        let list = t.ok("GET", "/api/changesets", None).await;
        let Some(c) = list.as_array().unwrap().first().cloned() else {
            continue;
        };
        let cs_key = c["_key"].as_str().unwrap().to_string();
        let full = t
            .ok("GET", &format!("/api/changesets/{cs_key}"), None)
            .await;
        if let Some(e) = full["error"].as_str() {
            panic!(
                "agent error: {e}\n{}",
                serde_json::to_string_pretty(&full["activity"]).unwrap()
            );
        }
        match full["state"].as_str().unwrap() {
            "planned" if !approved => {
                eprintln!("plan: {}", full["plan"]["summary"]);
                t.uok("POST", &format!("/api/changesets/{cs_key}/approve"), None)
                    .await;
                approved = true;
            }
            "merged" | "escalated" | "rejected" => break full,
            s => eprintln!("state: {s}"),
        }
    };
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap();
    eprintln!("activity: {}", pretty(&cs["activity"]));
    assert_eq!(
        cs["state"], "merged",
        "changeset {} ended {}",
        cs["_key"], cs["state"]
    );
    // a reviewer ran blind, at least one review, tests passed before merge
    assert!(!cs["reviews"].as_array().unwrap().is_empty());
    for tr in cs["transcripts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["role"] == "reviewer")
    {
        for c in cs["transcripts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["role"] == "coder")
        {
            if let Some(n) = c["output"]["notes"].as_str().filter(|n| n.len() > 20) {
                assert!(
                    !tr["input"].as_str().unwrap().contains(n),
                    "reviewer saw coder notes"
                );
            }
        }
    }
    assert!(cs["test_results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["status"] == "pass"));
    let cov = t.ok("GET", "/api/coverage", None).await;
    for r in cov["requirements"].as_array().unwrap() {
        assert_eq!(r["status"], "pass", "{}", r);
    }
    t.stop().await;
}
