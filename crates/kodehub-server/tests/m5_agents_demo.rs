//! M5 acceptance: the doc 08 demo end to end with recorded LLM responses (deterministic, no key).
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use kodehub_agents::engine::default_recording;
use kodehub_agents::{EngineOptions, RecordedBackend, Role};
use serde_json::{json, Value};

async fn wait_state(t: &TestServer, cs: &str, want: &[&str], secs: u64) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let v = t.ok("GET", &format!("/api/changesets/{cs}"), None).await;
        let st = v["state"].as_str().unwrap_or("").to_string();
        if want.contains(&st.as_str()) {
            return v;
        }
        if let Some(e) = v["error"].as_str() {
            panic!(
                "changeset error in state {st}: {e}\nactivity: {}",
                v["activity"]
            );
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out in state {st}; activity: {}",
            v["activity"]
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
async fn demo_scenario_with_recorded_agents() {
    let rec = Arc::new(RecordedBackend::from_file(&default_recording()).unwrap());
    let r2 = rec.clone();
    let t = TestServer::start_with("m5", move |st, _url| {
        kodehub_server::install_agents(st, "http://127.0.0.1:1", EngineOptions::default());
        let e = Arc::try_unwrap(st.engine.take().unwrap()).ok().unwrap();
        st.engine = Some(Arc::new(e.with_backend(r2)));
    })
    .await;

    // 1. everything green
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    let r = t.ok("POST", "/api/tests/run", None).await;
    assert_eq!(r["summary"]["failed"], 0);
    let cov = t.ok("GET", "/api/coverage", None).await;
    assert!(cov["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["status"] == "pass"));

    // 2. human adds an edge case to R1
    let edit = t
        .uok(
            "POST",
            "/api/requirements/r1-generic-failure/edge_cases",
            Some(json!({
        "description": "A locked account must also return invalid_credentials",
        "input": {"username": "alice", "password": "correct horse", "locked": true},
        "expected": "invalid_credentials"})),
        )
        .await;
    assert_eq!(edit["changed"], true);

    // 3. R1 and everything beneath it turns amber; a changeset is opened and planned with ghost nodes
    let cov = t
        .ok("GET", "/api/coverage?requirement=r1-generic-failure", None)
        .await;
    assert_eq!(cov["requirements"][0]["status"], "at_risk");
    let health = t.ok("GET", "/api/health/nodes", None).await;
    assert_eq!(health["symbols/verify_credentials"]["health"], "at_risk");
    let mut cs_key = String::new();
    for _ in 0..80 {
        let list = t.ok("GET", "/api/changesets?open=true", None).await;
        if let Some(c) = list
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["state"] == "planned")
        {
            cs_key = c["_key"].as_str().unwrap().to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        !cs_key.is_empty(),
        "orchestrator should open and plan a changeset"
    );
    let map = t.ok("GET", "/api/map", None).await;
    let ghosts: Vec<&Value> = map["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["ghost"] == true)
        .collect();
    assert!(
        ghosts
            .iter()
            .any(|g| g["label"] == "test_locked_account_is_invalid"),
        "ghost node for planned test"
    );
    assert!(map["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["id"] == "symbols/verify_credentials" && n["proposed_in"].is_array()));

    // 4. human approves → 5–7. tester, coder, blind reviewer, rework, decision, tests, merge
    t.uok("POST", &format!("/api/changesets/{cs_key}/approve"), None)
        .await;
    let cs = wait_state(&t, &cs_key, &["merged", "escalated"], 600).await;
    assert_eq!(cs["state"], "merged", "activity: {}", cs["activity"]);

    // blind review: the reviewer's prompts contain none of the coder's messages
    let calls = rec.calls();
    let coder_outputs: Vec<Value> = cs["transcripts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["role"] == "coder")
        .map(|t| t["output"].clone())
        .collect();
    assert_eq!(coder_outputs.len(), 2);
    let mut coder_messages: Vec<String> = Vec::new();
    for o in &coder_outputs {
        coder_messages.push(o["notes"].as_str().unwrap().to_string());
        for r in o["responses"].as_array().unwrap() {
            coder_messages.push(r["explanation"].as_str().unwrap().to_string());
        }
    }
    let reviewer_calls: Vec<_> = calls.iter().filter(|c| c.role == Role::Reviewer).collect();
    assert_eq!(reviewer_calls.len(), 2);
    for rc in &reviewer_calls {
        for m in &coder_messages {
            assert!(
                !rc.input.contains(m.as_str()) && !rc.system.contains(m.as_str()),
                "reviewer saw coder message: {m}"
            );
        }
        assert!(
            !rc.input.contains("PRIVATE-CODER"),
            "reviewer input leaked coder text"
        );
    }
    // ...but it did see the diff and the tests
    assert!(reviewer_calls[0]
        .input
        .contains("MODIFIED `verify_credentials`"));
    assert!(reviewer_calls[0]
        .input
        .contains("test_locked_account_is_invalid"));
    // the tester never saw function bodies
    let tester = calls.iter().find(|c| c.role == Role::Tester).unwrap();
    assert!(!tester
        .input
        .contains("verify_password(password, &user.password_hash)"));

    // reviewer raised at least one finding (timing), a Decision node records the outcome
    let first_review = &cs["reviews"][0]["findings"];
    assert!(first_review
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["severity"] == "blocking" && f["finding"].as_str().unwrap().contains("Timing")));
    assert!(!cs["decisions"].as_array().unwrap().is_empty());
    let decisions = t.ok("GET", "/api/decisions", None).await;
    let d = decisions
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["title"].as_str().unwrap().starts_with("Review: Timing"))
        .expect("decision recorded");
    assert!(d["justifies"]
        .as_array()
        .unwrap()
        .contains(&json!("symbols/verify_credentials")));
    assert!(d["choice"].as_str().unwrap().contains("Argon2"));

    // merge happened only after tests passed
    let states: Vec<&str> = cs["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["state"].as_str().unwrap())
        .collect();
    let ti = states.iter().rposition(|s| *s == "testing").unwrap();
    let ri = states.iter().position(|s| *s == "ready").unwrap();
    let mi = states.iter().position(|s| *s == "merged").unwrap();
    assert!(ti < ri && ri < mi, "{states:?}");
    assert!(cs["test_results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["status"] == "pass"));
    assert!(cs["test_results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "test_locked_account_is_invalid"));
    assert_eq!(cs["checks"]["orphans_ok"], true);

    // coverage is green afterwards
    let cov = t.ok("GET", "/api/coverage", None).await;
    for r in cov["requirements"].as_array().unwrap() {
        assert_eq!(r["status"], "pass", "{}", r);
    }
    assert_eq!(cov["groups"][0]["status"], "pass");
    let health = t.ok("GET", "/api/health/nodes", None).await;
    assert_eq!(health["contexts/authentication"]["health"], "pass");
    assert_eq!(t.ok("GET", "/api/invariants", None).await["ok"], true);

    // 8. run now with a locked user: invalid_credentials, instantly (cache hit — warmed after merge)
    let mut hit = Value::Null;
    for _ in 0..120 {
        let h = t
            .ok(
                "POST",
                "/api/run",
                Some(json!({"symbol": "hash_password", "args": {"password": "pw"}})),
            )
            .await;
        let args = json!({"store": {"users": {"alice": {"username": "alice", "password_hash": h["result"], "locked": true}}},
                          "username": "alice", "password": "pw"});
        hit = t
            .ok(
                "POST",
                "/api/run",
                Some(json!({"symbol": "verify_credentials", "args": args})),
            )
            .await;
        if hit["cache_hit"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(hit["result"], "invalid_credentials");
    // ...and the same user, unlocked, authenticates (the locked check is what rejects)
    let h = t
        .ok(
            "POST",
            "/api/run",
            Some(json!({"symbol": "hash_password", "args": {"password": "correct horse"}})),
        )
        .await;
    let unlocked = t.ok("POST", "/api/run", Some(json!({"symbol": "verify_credentials", "args": {
        "store": {"users": {"alice": {"username": "alice", "password_hash": h["result"], "locked": false}}},
        "username": "alice", "password": "correct horse"}}))).await;
    assert_eq!(
        unlocked["result"],
        json!({"authenticated": {"username": "alice"}}),
        "{unlocked}"
    );
    assert_eq!(hit["cache_hit"], true);
    assert!(hit["duration_ms"].as_u64().unwrap() < 1000);
    assert_eq!(
        rec.remaining(),
        4,
        "the demo consumed its 6 responses; 4 remain for the optional save-as-example cycle"
    );
    // re-importing the seed over the changed graph works (the UI e2e suite does this)
    let (s, v) = t.req("POST", "/api/seed", Some(json!({"path": seed_path()}))).await;
    assert_eq!(s, 200, "re-seed after the demo: {v}");
    t.stop().await;
}
