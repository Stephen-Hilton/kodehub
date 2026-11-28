//! S8 acceptance (agent containers): with `KODEHUB_AGENT_CONTAINERS=always` every agent task runs
//! in its own container (here the local runtime: the same `kodehub-worker agent` entrypoint as a
//! subprocess) and returns through storage; nothing of the task survives it.
mod common;

use common::*;
use kodehub_agents::EngineOptions;
use kodehub_server::orchestrator::AgentDispatcher;
use serde_json::json;

#[tokio::test]
async fn agent_tasks_run_in_their_own_container() {
    let t = TestServer::start_with("s8ctr", |st, _| {
        st.dispatch = false;
        kodehub_server::install_agents(st, "http://127.0.0.1:1", EngineOptions::default());
    })
    .await;
    // the container finds storage and the recording through its environment
    std::env::set_var("KODEHUB_AGENT_CONTAINERS", "always");
    std::env::set_var("KODEHUB_HOME", &t.state.runner.home);
    std::env::set_var(
        "KODEHUB_RECORDING",
        repo().join("crates/kodehub-agents/recordings/demo-locked-account.json"),
    );
    std::env::set_var(
        "KODEHUB_WORKER_BIN",
        repo().join("target/debug/kodehub-worker"),
    );
    let pid = t.store.project_id().unwrap();
    t.uok(
        "PUT",
        &format!("/api/scopes/project:{pid}/settings/agent_backend"),
        Some(json!({"value": "recorded"})),
    )
    .await;
    t.uok(
        "POST",
        "/api/work-items",
        Some(json!({"agent_type": "planning", "kind": "smoke", "target": "philosophy/philosophy"})),
    )
    .await;
    let d = AgentDispatcher {
        store: t.store.clone(),
        engine: t.state.engine.clone().unwrap(),
        owner: "ctr".into(),
        events: t.state.events.clone(),
    };
    assert!(d.tick().await.unwrap());
    let items = t.store.list_work_items(Some(&pid), None, 10).await.unwrap();
    assert_eq!(items[0]["state"], "done", "{}", items[0]);
    assert_eq!(items[0]["result"]["backend"], "recorded-in-container");
    // each container is fresh, but the recording keeps its place across tasks: a second task
    // of the same role gets the recording's second answer, not the first again
    t.uok(
        "POST",
        "/api/work-items",
        Some(json!({"agent_type": "planning", "kind": "smoke", "target": "philosophy/philosophy"})),
    )
    .await;
    assert!(d.tick().await.unwrap());
    let items = t.store.list_work_items(Some(&pid), None, 10).await.unwrap();
    assert!(items.iter().all(|w| w["state"] == "done"), "{items:?}");
    assert_eq!(
        t.state.engine.as_ref().unwrap().recording_remaining(&pid),
        Some(8),
        "two container tasks took the recording's first two orchestrator answers"
    );
    // the task input (which carried a token) is gone from storage
    let objs = kodehub_storage::StorageClient::from_env_or_local(&t.state.runner.home)
        .list(kodehub_storage::BUCKET, &format!("projects/{pid}/jobs/"))
        .await
        .unwrap();
    assert!(
        objs.iter().all(|o| !o.key.ends_with("input.json")),
        "{objs:?}"
    );
    t.stop().await;
}
