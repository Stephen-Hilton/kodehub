//! Spec §11.4–11.5: the sysadmin check sleeps when all is well and otherwise queues a sysadmin
//! agent that sets caps; a test budget too small for the default cadence stretches the tiers
//! while the daily project run is kept.
mod common;

use std::sync::Arc;

use common::*;
use kodehub_agents::{AgentBackend, AgentOutput, AgentTask, EngineOptions};
use kodehub_runner::jobs::JobRuntime;
use kodehub_server::orchestrator::{AgentDispatcher, Orchestrator};
use serde_json::json;

struct Sysadmin;

#[async_trait::async_trait]
impl AgentBackend for Sysadmin {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn run(&self, task: &AgentTask) -> kodehub_agents::Result<AgentOutput> {
        assert!(task.label.starts_with("sysadmin"), "{}", task.label);
        assert!(task.input.contains("waiting_agent_items"));
        let json = json!({"change_log": "spare capacity: allow more coding agents; watch CPU", "actions": [
            {"type": "set_caps", "agent_caps": {"coding": 6, "test_author": 2, "review": 2, "planning": 2,
                                                 "architect": 1, "import": 1, "design": 1, "sysadmin": 1}}]});
        Ok(AgentOutput {
            text: json.to_string(),
            json,
            transcript: vec![],
            backend: "scripted".into(),
            model: "m".into(),
            duration_ms: 0,
            usage: json!({}),
        })
    }
}

#[tokio::test]
async fn sysadmin_wakes_only_when_needed_and_budget_thins_the_cadence() {
    let t = TestServer::start_with("s11sys", |st, _| {
        st.dispatch = false;
        kodehub_server::install_agents(st, "http://127.0.0.1:1", EngineOptions::default());
        let e = Arc::try_unwrap(st.engine.take().unwrap()).ok().unwrap();
        st.engine = Some(Arc::new(e.with_backend(Arc::new(Sysadmin))));
    })
    .await;
    let o = Orchestrator {
        store: t.store.clone(),
        runner: t.state.runner.clone(),
        storage: kodehub_storage::StorageClient::from_env_or_local(&t.state.runner.home),
        jobs: JobRuntime::Local {
            worker: repo().join("target/debug/kodehub-worker"),
            env: vec![],
        },
        owner: "o".into(),
        events: t.state.events.clone(),
    };
    let pid = t.store.project_id().unwrap();
    // all is well: back to sleep
    assert!(o.sysadmin_check().await.unwrap().is_none());
    // coding work held back by a cap of 0 → a sysadmin agent is queued, once
    t.uok(
        "PUT",
        &format!("/api/scopes/project:{pid}/settings/agent_caps"),
        Some(json!({"value": {"coding": 0, "sysadmin": 1}})),
    )
    .await;
    for i in 0..3 {
        t.ok("POST", "/api/work-items", Some(json!({"agent_type": "coding", "kind": "implement", "target": format!("symbols/s{i}")}))).await;
    }
    let w = o.sysadmin_check().await.unwrap().expect("sysadmin queued");
    assert_eq!(w["agent_type"], "sysadmin");
    assert_eq!(w["task"]["waiting_agent_items"], 3);
    assert!(o.sysadmin_check().await.unwrap().is_none(), "one at a time");
    // the agent forms a hypothesis and applies it as app-level caps
    let d = AgentDispatcher {
        store: t.store.clone(),
        engine: t.state.engine.clone().unwrap(),
        owner: "a".into(),
        events: t.state.events.clone(),
    };
    assert!(d.tick().await.unwrap());
    let app = t.uok("GET", "/api/scopes/app/settings", None).await;
    assert_eq!(app["stored"]["agent_caps"]["coding"], 6);

    // a tiny test budget stretches the tiers, never the daily run
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    t.ok("POST", "/api/tests/run", None).await;
    std::env::set_var("KODEHUB_CLUSTER_CPUS", "1");
    t.uok(
        "PUT",
        &format!("/api/scopes/project:{pid}/settings/test_budget_pct"),
        Some(json!({"value": 0.001})),
    )
    .await;
    let scheds = t.ok("POST", "/api/schedules/replan", None).await;
    let comp = scheds
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["level"] == "component")
        .expect("a component schedule");
    assert!(comp["interval_secs"].as_i64().unwrap() > 7200, "{comp}");
    let daily = scheds
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["target"] == "project")
        .unwrap();
    assert_eq!(daily["interval_secs"], 86400);
    t.stop().await;
}
