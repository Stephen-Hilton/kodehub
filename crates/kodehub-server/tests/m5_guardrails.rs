//! Agent guardrails from the critical reviews: the coder may not rewrite tests (M6 #5, final #12)
//! and changes outside the approved plan need human re-approval (M6 #13, final #11).
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use kodehub_agents::{EngineOptions, RecordedBackend};
use serde_json::{json, Value};

const VC_BODY: &str = "match store.find_user(username) {\n    Some(user) if verify_password(password, &user.password_hash) => {\n        AuthResult::Authenticated {\n            username: user.username,\n        }\n    }\n    _ => AuthResult::InvalidCredentials,\n}";

async fn state_of(t: &TestServer, k: &str) -> Value {
    t.ok("GET", &format!("/api/work-items/{k}/proposal"), None)
        .await
}

async fn wait(t: &TestServer, k: &str, want: &str) -> Value {
    for _ in 0..240 {
        let v = state_of(t, k).await;
        if v["state"] == want {
            return v;
        }
        if let Some(e) = v["error"].as_str() {
            panic!("error: {e}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("timed out waiting for {want}: {}", state_of(t, k).await);
}

#[tokio::test]
async fn coder_cannot_touch_tests_and_out_of_plan_changes_need_reapproval() {
    let rec = json!({"scenario": "guardrails", "responses": [
        {"role": "orchestrator", "output": {"summary": "comment verify_credentials",
            "changes": [{"symbol": "verify_credentials", "display_name": "verify_credentials", "kind": "function", "language": "rust", "action": "modify", "component": "", "description": "doc comment"},
                        {"symbol": "", "display_name": "test_whitespace_username", "kind": "test", "language": "rust", "action": "add", "component": "credential-verification", "description": "edge case"}],
            "tests": [], "risks": []}},
        {"role": "tester", "output": {"tests": [{"display_name": "test_whitespace_username", "language": "rust",
            "body": "let mut store = UserStore::new();\nstore.add_user(\"alice\", \"x\");\nassert_eq!(verify_credentials(&store, \" \", \"x\"), AuthResult::InvalidCredentials);",
            "verifies": ["r1-generic-failure"], "tests_for": ["verify_credentials"], "description": "edge"}], "notes": ""}},
        {"role": "coder", "output": {"changes": [
            {"symbol": "verify_credentials", "display_name": "verify_credentials", "kind": "function", "language": "rust",
             "signature": "(store: &UserStore, username: &str, password: &str) -> AuthResult", "body": format!("// generic failures only\n{VC_BODY}"),
             "attributes_json": "", "component": "", "member_of": "", "description": ""},
            // tries to weaken a test through the name-lookup path (symbol "", kind function)
            {"symbol": "", "display_name": "test_wrong_password_is_invalid", "kind": "function", "language": "rust",
             "signature": "", "body": "assert!(true);", "attributes_json": "", "component": "", "member_of": "", "description": ""},
            // modifies an existing symbol that is not in the plan
            {"symbol": "hash_password", "display_name": "hash_password", "kind": "function", "language": "rust",
             "signature": "(password: &str) -> String",
             "body": "// hardened\nlet salt = SaltString::generate(&mut OsRng);\nArgon2::default()\n    .hash_password(password.as_bytes(), &salt)\n    .expect(\"argon2 hashing with default parameters cannot fail\")\n    .to_string()",
             "attributes_json": "", "component": "", "member_of": "", "description": ""}],
          "responses": [], "notes": ""}},
        {"role": "reviewer", "output": {"verdict": "approve", "summary": "ok", "findings": []}}
    ]});
    let backend = Arc::new(RecordedBackend::from_value(&rec).unwrap());
    let b2 = backend.clone();
    let t = TestServer::start_with("guard", move |st, _| {
        kodehub_server::install_agents(st, "http://127.0.0.1:1", EngineOptions::default());
        let e = Arc::try_unwrap(st.engine.take().unwrap()).ok().unwrap();
        st.engine = Some(Arc::new(e.with_backend(b2)));
    })
    .await;
    // re-approval of out-of-plan changes is part of the human-approves-plans flow
    let pid = t.store.project_id().unwrap();
    t.uok(
        "PUT",
        &format!("/api/scopes/project:{pid}/settings/human_engagement"),
        Some(json!({"value": "plans"})),
    )
    .await;
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    t.ok("POST", "/api/tests/run", None).await;
    t.uok("POST", "/api/requirements/r1-generic-failure/edge_cases", Some(json!({"description": "whitespace username", "input": {"username": " "}, "expected": "invalid_credentials"}))).await;
    let mut k = String::new();
    for _ in 0..80 {
        if let Some(c) = t
            .ok("GET", "/api/proposals?open=true", None)
            .await
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["state"] == "planned")
        {
            k = c["_key"].as_str().unwrap().to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(!k.is_empty());
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/approve"),
        None,
    )
    .await;

    // the coder touched hash_password outside the plan → back to planned for re-approval
    for _ in 0..120 {
        let v = state_of(&t, &k).await;
        if v["state"] == "planned" && v["skip_to_review"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let cs = state_of(&t, &k).await;
    assert_eq!(cs["state"], "planned", "{}", cs["activity"]);
    assert!(cs["plan"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["out_of_plan"] == true && c["display_name"] == "hash_password"));
    // the test rewrite was refused: no proposal touches the existing test
    assert!(cs["proposals"]
        .get(t.q("test_wrong_password_is_invalid"))
        .is_none());
    assert!(cs["activity"]
        .to_string()
        .contains("Refused coder changes to tests"));

    // human re-approves → straight to review (proposals unchanged) → tests → acceptance
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/approve"),
        None,
    )
    .await;
    let done = wait(&t, &k, "accepted").await;
    assert!(done["accepted_versions"]
        .get(t.q("hash_password"))
        .is_some());
    assert_eq!(backend.remaining(), 0);
    // R1's new edge case is covered by the accepted test → green
    let cov = t
        .ok("GET", "/api/coverage?requirement=r1-generic-failure", None)
        .await;
    assert_eq!(cov["requirements"][0]["status"], "pass");
    let r1 = t
        .ok("GET", "/api/requirements/r1-generic-failure", None)
        .await;
    assert_eq!(r1["covered_revision"], r1["revision"]);
    t.stop().await;
}
