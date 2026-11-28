//! Ontology step 2 (D-061): remote agents are their own kind of identity. They may add work
//! items, edit requirements and start tests; code, builds and deployments are changed only by
//! Kodehub's internal agents.
mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn remote_agents_direct_work_but_never_change_code() {
    let t = TestServer::start("o2remote").await;
    t.uok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;

    // the agent user lives in remote_agents, not users, and acts for the human
    let me = t.store.authenticate(&t.pat).await.unwrap();
    assert!(me.is_remote_agent());
    assert!(me.id().starts_with("remote_agents/"), "{}", me.id());
    assert_eq!(me.on_behalf_of().as_deref(), Some("users/tester"));
    let in_users = t
        .store
        .db
        .aql_raw(
            "RETURN COUNT(FOR u IN users FILTER u._key == @k RETURN 1)",
            json!({"k": me.user}),
        )
        .await
        .unwrap();
    assert_eq!(in_users[0], 0);
    let users = t.uok("GET", "/api/users", None).await;
    assert!(users
        .as_array()
        .unwrap()
        .iter()
        .all(|u| u["key"] != me.user.as_str()));

    // allowed: add work, edit requirements, start tests, read anything
    let (s, _) = t
        .req(
            "POST",
            "/api/work-items",
            Some(
                json!({"agent_type": "coding", "kind": "task", "target": "symbols/hash_password",
                        "task": {"note": "from a remote agent"}}),
            ),
        )
        .await;
    assert_eq!(s, 200);
    let (s, v) = t
        .req(
            "POST",
            "/api/requirements/r1-generic-failure/edge_cases",
            Some(json!({"description": "Empty password", "input": {}, "expected": "invalid_credentials"})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (s, _) = t
        .req(
            "POST",
            "/api/tests/run",
            Some(json!({"scope": {"kind": "symbol", "id": "hash_password"}})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = t.req("GET", "/api/map", None).await;
    assert_eq!(s, 200);

    // refused: code, builds, releases and deployments
    for (m, path, body) in [
        ("POST", "/api/seed", json!({"path": seed_path()})),
        ("POST", "/api/changesets", json!({"title": "x"})),
        (
            "POST",
            "/api/manual-edit",
            json!({"symbol": "hash_password", "body": "x"}),
        ),
        ("POST", "/api/releases", json!({"name": "x"})),
        ("POST", "/api/environments/test/promote", json!({})),
        (
            "POST",
            "/api/edges/calls",
            json!({"from": "symbols/hash_password", "to": "symbols/verify_password"}),
        ),
        ("POST", "/api/git/import", json!({"repo_url": "file:///x"})),
        ("PUT", "/api/philosophy", json!({"text": "x"})),
    ] {
        let (s, v) = t.req(m, path, Some(body)).await;
        assert_eq!(s, 403, "{m} {path} must be refused for a remote agent: {v}");
    }
    t.stop().await;
}
