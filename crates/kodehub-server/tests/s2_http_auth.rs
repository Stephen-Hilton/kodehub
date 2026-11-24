//! S2 acceptance over HTTP: auth required, request-scoped projects, isolation, revocation,
//! human-only actions, audit by actor class.
mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn projects_are_isolated_per_request_and_audited() {
    let t = TestServer::start("s2http").await;
    // health is public, the API is not
    let (s, _) = t.req_as(None, "GET", "/health", None).await;
    assert_eq!(s, 200);
    let (s, _) = t.req_as(None, "GET", "/api/map", None).await;
    assert_eq!(s, 401);

    // tester creates a second project with a requirement in it
    let p = t
        .uok(
            "POST",
            "/api/projects",
            Some(
                json!({"name": "Second", "philosophy": "Ship small.", "contexts": ["Web System"]}),
            ),
        )
        .await;
    let second = p["id"].as_str().unwrap().to_string();
    assert_eq!(p["initialized"]["contexts"].as_array().unwrap().len(), 1);

    // bob registers; he sees no projects and cannot read tester's
    let (_, reg) = t
        .req_as(
            None,
            "POST",
            "/api/auth/register",
            Some(json!({"username": "bob", "password": "password123"})),
        )
        .await;
    let bob = reg["token"].as_str().unwrap().to_string();
    let (_, list) = t.req_as(Some(&bob), "GET", "/api/projects", None).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
    let (s, _) = t
        .req_as(
            Some(&bob),
            "GET",
            &format!("/api/requirements?project={second}"),
            None,
        )
        .await;
    assert_eq!(s, 403);

    // concurrent requests for two projects each see only their own data
    let first = t.store.project_id().unwrap();
    let (u1, u2) = (
        format!("/api/nodes/contexts?project={first}"),
        format!("/api/nodes/contexts?project={second}"),
    );
    let r1 = t.req_as(Some(&t.session), "GET", &u1, None);
    let r2 = t.req_as(Some(&t.session), "GET", &u2, None);
    let ((s1, a), (s2, b)) = tokio::join!(r1, r2);
    assert_eq!((s1, s2), (200, 200));
    assert!(
        a.as_array().unwrap().is_empty(),
        "first project has no contexts: {a}"
    );
    assert_eq!(b.as_array().unwrap()[0]["name"], "Web System");
    let (_, mission) = t
        .req_as(
            Some(&t.session),
            "GET",
            &format!("/api/missions?project={second}"),
            None,
        )
        .await;
    assert!(mission.to_string().contains("Ship small."));

    // share read-only with bob: he can read but not write
    t.uok(
        "PUT",
        &format!("/api/projects/{second}/shares"),
        Some(json!({"grantee": "user:bob", "permission": "read"})),
    )
    .await;
    let (s, _) = t
        .req_as(
            Some(&bob),
            "GET",
            &format!("/api/nodes/contexts?project={second}"),
            None,
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = t
        .req_as(
            Some(&bob),
            "POST",
            &format!("/api/nodes/contexts?project={second}"),
            Some(json!({"name": "Sneaky"})),
        )
        .await;
    assert_eq!(s, 403);

    // an agent's token cannot perform human-only actions even with access
    let (s, _) = t
        .req(
            "POST",
            "/api/projects",
            Some(json!({"name": "agent project"})),
        )
        .await;
    assert_eq!(s, 403);
    let (s, _) = t
        .req(
            "POST",
            "/api/tokens",
            Some(json!({"agent": "x", "name": "y"})),
        )
        .await;
    assert_eq!(s, 403);

    // revoking the PAT takes effect on the next call
    let (s, _) = t.req("GET", "/api/map", None).await;
    assert_eq!(s, 200);
    let toks = t.uok("GET", "/api/tokens", None).await;
    let id = toks[0]["id"].as_str().unwrap();
    t.uok("DELETE", &format!("/api/tokens/{id}"), None).await;
    let (s, _) = t.req("GET", "/api/map", None).await;
    assert_eq!(s, 401);

    // settings over HTTP: project admin sets, reader cannot
    t.uok(
        "PUT",
        &format!("/api/scopes/project:{second}/settings/retry_limit"),
        Some(json!({"value": 1})),
    )
    .await;
    let v = t
        .uok(
            "GET",
            &format!("/api/scopes/project:{second}/settings"),
            None,
        )
        .await;
    assert_eq!(v["resolved"]["retry_limit"]["value"], 1);
    let (s, _) = t
        .req_as(
            Some(&bob),
            "PUT",
            &format!("/api/scopes/project:{second}/settings/retry_limit"),
            Some(json!({"value": 9})),
        )
        .await;
    assert_ne!(s, 200);

    // audit: tester's actions are human, the agent's are external_agent
    let rows = t.uok("GET", "/api/audit?class=external_agent", None).await;
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["actor_class"] == "external_agent"));
    let rows = t
        .uok("GET", &format!("/api/audit?project={second}"), None)
        .await;
    assert!(rows
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["action"].as_str().unwrap().contains("PUT /api/scopes")));
    assert!(!rows.to_string().contains("khs_") && !rows.to_string().contains("khp_"));
    t.stop().await;
}
