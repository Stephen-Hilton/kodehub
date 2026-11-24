//! S8 acceptance (no model credentials: a scripted backend): the kickoff chain runs through the
//! queue (architect → planning per context → planning per container → tests before code →
//! coding), agents are one ephemeral task per step with a project-scoped token, usage is
//! metered, a third failed rework escalates to an architect, external blockers reach a human,
//! a project at budget claims nothing, and a subscription token never reaches the API backend.
mod common;

use std::sync::{Arc, Mutex};

use common::*;
use kodehub_agents::{AgentBackend, AgentError, AgentOutput, AgentTask, EngineOptions};
use kodehub_server::orchestrator::AgentDispatcher;
use serde_json::{json, Value};

/// Answers by task label; records every task it saw.
struct Scripted {
    seen: Mutex<Vec<AgentTask>>,
}

#[async_trait::async_trait]
impl AgentBackend for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn run(&self, task: &AgentTask) -> kodehub_agents::Result<AgentOutput> {
        self.seen.lock().unwrap().push(task.clone());
        let l = task.label.as_str();
        let json = if l.starts_with("architect: cleanup_philosophy") {
            json!({"change_log": "tidied the transcript", "actions": [
                {"type": "set_philosophy", "text": "Sign-in is fast, private and never leaks why it failed."}]})
        } else if l.starts_with("architect: evaluate_requirements") {
            json!({"change_log": "one gap", "actions": [
                {"type": "propose_requirement_change", "requirement": null,
                 "requirement_doc": {"title": "Sessions expire", "statement": "Sessions expire after 30 minutes idle.", "kind": "business"},
                 "attach_to": "contexts/web-system", "rationale": "the philosophy asks for privacy"}]})
        } else if l.starts_with("planning: plan_context") {
            json!({"change_log": "one container", "actions": [
                {"type": "create_nodes", "nodes": [{"level": "container", "name": "web-api", "parent": "contexts/web-system", "description": "HTTP API"}]},
                {"type": "enqueue", "items": [{"agent_type": "planning", "kind": "plan_container", "target": "containers/web-api"}]}]})
        } else if l.starts_with("planning: plan_container") {
            json!({"change_log": "one component, tests first", "actions": [
                {"type": "create_nodes", "nodes": [{"level": "component", "name": "sessions", "parent": "containers/web-api"}]},
                {"type": "enqueue", "items": [
                    {"agent_type": "test_author", "kind": "write_tests", "target": "components/sessions"},
                    {"agent_type": "coding", "kind": "implement_component", "target": "components/sessions", "after": [0]}]}]})
        } else if l.starts_with("test_author: write_tests") {
            json!({"change_log": "a test from the requirement", "actions": [
                {"type": "write_tests", "component": "components/sessions", "tests": [
                    {"display_name": "test_session_expires_after_idle", "language": "rust",
                     "body": "assert!(session_expired(31));", "verifies": ["sessions-idle"]}]}]})
        } else if l.starts_with("architect: resolve_hard_failure") {
            json!({"change_log": "not a code problem", "actions": [
                {"type": "external_blocker", "message": "the session store's API key was revoked"}]})
        } else {
            // the coding engine's planning step: fail, to exercise retries and escalation
            return Err(AgentError::Backend(format!("scripted failure for `{l}`")));
        };
        Ok(AgentOutput {
            text: json.to_string(),
            json,
            transcript: vec![],
            backend: "scripted".into(),
            model: "claude-sonnet-5-5".into(),
            duration_ms: 1,
            usage: json!({"input_tokens": 1000, "output_tokens": 200}),
        })
    }
}

async fn drain(d: &AgentDispatcher, max: usize) -> usize {
    let mut n = 0;
    while n < max && d.tick().await.unwrap() {
        n += 1;
    }
    n
}

#[tokio::test]
async fn kickoff_chain_tests_first_escalation_blockers_budget() {
    let backend = Arc::new(Scripted {
        seen: Mutex::new(vec![]),
    });
    let b2 = backend.clone();
    let t = TestServer::start_with("s8", move |st, _| {
        st.dispatch = false;
        kodehub_server::install_agents(st, "http://127.0.0.1:1", EngineOptions::default());
        let e = Arc::try_unwrap(st.engine.take().unwrap()).ok().unwrap();
        st.engine = Some(Arc::new(e.with_backend(b2)));
    })
    .await;
    let d = AgentDispatcher {
        store: t.store.clone(),
        engine: t.state.engine.clone().unwrap(),
        owner: "test-dispatcher".into(),
        events: t.state.events.clone(),
    };
    // a new project from a spoken philosophy and one starting context
    let p = t
        .uok(
            "POST",
            "/api/projects",
            Some(json!({
            "name": "Shop", "raw_transcript": "um so sign in should be like fast and uh private",
            "contexts": ["Web System"]})),
        )
        .await;
    let pid = p["id"].as_str().unwrap().to_string();
    assert_eq!(
        p["kickoff"].as_array().unwrap().len(),
        3,
        "cleanup → evaluate → plan per context"
    );
    // a requirement the tests will verify
    let hdr = |path: &str| {
        format!(
            "{path}{}project={pid}",
            if path.contains('?') { "&" } else { "?" }
        )
    };
    t.uok(
        "POST",
        &hdr("/api/requirements"),
        Some(
            json!({"_key": "sessions-idle", "title": "Idle sessions end",
        "statement": "A session idle for 30 minutes expires.", "kind": "technical"}),
        ),
    )
    .await;
    // (test setup: trace edges otherwise come through changesets)
    t.store
        .for_project(&pid)
        .db
        .insert(
            "satisfies",
            &json!({"_from": "contexts/web-system", "_to": "requirements/sessions-idle"}),
            None,
        )
        .await
        .unwrap();

    drain(&d, 6).await;
    let ps = t.store.for_project(&pid);
    // 1. the architect cleaned up the transcript
    let ph = ps.get_philosophy().await.unwrap().unwrap();
    assert!(ph["text"].as_str().unwrap().starts_with("Sign-in is fast"));
    assert!(ph["raw_transcript"].as_str().unwrap().contains("um so"));
    // 2. its requirement proposal waits for a human
    let ap = ps.list_approvals(Some("pending")).await.unwrap();
    assert_eq!(ap.len(), 1);
    assert_eq!(ap[0]["kind"], "requirement_change");
    // 3. planning per context, then per container: a planned container and component
    let lc = kodehub_store::with_project(pid.clone(), ps.node_lifecycle())
        .await
        .unwrap();
    assert!(ps.get_node("containers/web-api").await.is_ok());
    assert_eq!(lc["components/sessions"]["state"], "planned");
    // 4. tests before code: the test exists for the planned component, the coding item came after it
    let tests = ps
        .aql(
            "FOR s IN @@symbols FILTER s.kind == 'test' RETURN s._key",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(tests, vec![json!("test_session_expires_after_idle")]);
    let items = t
        .store
        .list_work_items(Some(&pid), None, 100)
        .await
        .unwrap();
    let wt = items.iter().find(|w| w["kind"] == "write_tests").unwrap();
    let ic = items
        .iter()
        .find(|w| w["kind"] == "implement_component" && w["retry_of"].is_null())
        .unwrap();
    assert!(
        ic["depends_on"].as_array().unwrap().contains(&wt["_key"]),
        "code waits for tests"
    );
    assert_eq!(wt["state"], "done");

    // every agent task was one fresh session with a project-scoped token
    {
        let seen = backend.seen.lock().unwrap();
        assert!(seen.len() >= 5);
        assert!(seen
            .iter()
            .all(|x| x.project.as_deref() == Some(pid.as_str())
                && x.api_token
                    .as_deref()
                    .is_some_and(|k| k.starts_with("khi_"))));
    }
    // metered per project
    let spend = t.store.project_spend(&pid).await.unwrap();
    assert!(
        spend["calls"].as_u64().unwrap() >= 5 && spend["input_tokens"].as_u64().unwrap() >= 5000,
        "{spend}"
    );

    // 5. the coding item fails its retry limit (3) and escalates to an architect, who finds an
    //    external blocker: a human is told, clearly marked as unrelated to the code
    drain(&d, 10).await;
    let items = t
        .store
        .list_work_items(Some(&pid), None, 100)
        .await
        .unwrap();
    let failed = items
        .iter()
        .filter(|w| w["kind"] == "implement_component" && w["state"] == "failed")
        .count();
    assert_eq!(failed, 3, "three rework cycles");
    let arch = items
        .iter()
        .find(|w| w["kind"] == "resolve_hard_failure")
        .expect("escalated");
    assert_eq!(arch["state"], "blocked");
    assert_eq!(
        arch["history"].as_array().unwrap().len(),
        3,
        "the full history travelled"
    );
    let notes = t.store.notifications(Some(&pid), true).await.unwrap();
    assert!(notes
        .iter()
        .any(|n| n["kind"] == "external_blocker" && n["unrelated_to_code"] == true));

    // 6. a human approves the requirement change: it exists afterwards
    let ap_key = ap[0]["_key"].as_str().unwrap();
    t.uok(
        "POST",
        &hdr(&format!("/api/approvals/{ap_key}/decide")),
        Some(json!({"approve": true})),
    )
    .await;
    assert!(ps.db.get("requirements", "", None).await.is_err());
    let reqs = ps
        .aql(
            "FOR r IN @@requirements FILTER r.title == 'Sessions expire' RETURN r",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(reqs.len(), 1);
    // an agent cannot decide approvals
    let (s, _) = t
        .req(
            "POST",
            &hdr(&format!("/api/approvals/{ap_key}/decide")),
            Some(json!({"approve": true})),
        )
        .await;
    assert_eq!(s, 403);

    // 7. at budget, nothing new is claimed
    t.uok(
        "PUT",
        &format!("/api/scopes/project:{pid}/settings/budget_usd"),
        Some(json!(json!({"value": 0.0001}))),
    )
    .await;
    t.uok(
        "PUT",
        &format!("/api/scopes/project:{pid}/settings/budget_usd"),
        Some(json!({"value": 0.0001})),
    )
    .await;
    t.uok("POST", &hdr("/api/work-items"), Some(json!({"agent_type": "planning", "kind": "plan_context", "target": "contexts/web-system"}))).await;
    assert!(
        !d.tick().await.unwrap(),
        "a project at its budget claims no new work"
    );
    t.stop().await;
}

#[tokio::test]
async fn subscription_tokens_never_reach_the_api_backend() {
    let t = TestServer::start_with("s8cred", |st, _| {
        st.dispatch = false;
        kodehub_server::install_agents(st, "http://127.0.0.1:1", EngineOptions::default());
    })
    .await;
    let d = AgentDispatcher {
        store: t.store.clone(),
        engine: t.state.engine.clone().unwrap(),
        owner: "test-dispatcher".into(),
        events: t.state.events.clone(),
    };
    let pid = t.store.project_id().unwrap();
    std::env::remove_var("KODEHUB_ANTHROPIC_API_KEY");
    // the owner keeps only a subscription token; the project asks for the API backend
    t.uok("POST", "/api/credentials", Some(json!({"scope": "user:tester", "kind": "subscription_token", "secret": "sk-ant-oat01-personal"}))).await;
    t.uok(
        "PUT",
        &format!("/api/scopes/project:{pid}/settings/agent_backend"),
        Some(json!({"value": "api"})),
    )
    .await;
    t.uok("POST", "/api/work-items", Some(json!({"agent_type": "planning", "kind": "plan_context", "target": "philosophy/philosophy"}))).await;
    assert!(d.tick().await.unwrap());
    let items: Vec<Value> = t.store.list_work_items(Some(&pid), None, 10).await.unwrap();
    assert_eq!(items[0]["state"], "blocked", "{}", items[0]);
    assert!(items[0]["blocked_reason"]
        .as_str()
        .unwrap()
        .contains("API key"));
    let notes = t.store.notifications(Some(&pid), true).await.unwrap();
    assert_eq!(notes[0]["kind"], "external_blocker");
    // once a human adds a key, blocked work goes back to the queue
    let (s, _) = t.ui("POST", "/api/work-items/unblock", None).await;
    assert_eq!(s, 200);
    assert_eq!(
        t.store
            .list_work_items(Some(&pid), Some("queued"), 10)
            .await
            .unwrap()
            .len(),
        1
    );
    t.stop().await;
}
