//! M7 acceptance: releases and environments.
mod common;

use common::*;
use serde_json::{json, Value};

async fn edit(t: &TestServer, symbol: &str, f: impl Fn(&str) -> String) {
    // a direct version write (as a merged changeset would do) to create a difference
    let d = t.ok("GET", &format!("/api/symbols/{symbol}"), None).await;
    let content = kodehub_core::model::VersionContent {
        kind: serde_json::from_value(d["version"]["kind"].clone()).unwrap(),
        language: serde_json::from_value(d["version"]["language"].clone()).unwrap(),
        signature: d["version"]["signature"].as_str().unwrap().into(),
        body: f(d["version"]["body"].as_str().unwrap()),
        attributes: d["version"]["attributes"].clone(),
    };
    t.store
        .write_version(symbol, &content, "test")
        .await
        .unwrap();
}

fn running(envs: &Value, env: &str) -> Option<String> {
    envs.as_array()
        .unwrap()
        .iter()
        .find(|e| e["_key"] == env)
        .and_then(|e| e["currently_running"]["release"].as_str().map(String::from))
}

#[tokio::test]
async fn releases_are_immutable_promotable_rollbackable_and_diffable() {
    let t = TestServer::start("rel").await;
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    t.ok("POST", "/api/tests/run", None).await;

    // cut → running in dev
    let r1 = t
        .ok(
            "POST",
            "/api/releases",
            Some(json!({"name": "r1", "notes": "first"})),
        )
        .await;
    let k1 = r1["_key"].as_str().unwrap().to_string();
    assert!(r1["manifest"].as_array().unwrap().len() > 10);
    let envs = t.ok("GET", "/api/environments", None).await;
    assert_eq!(running(&envs, "dev").as_deref(), Some(k1.as_str()));
    assert_eq!(running(&envs, "test"), None);

    // immutable: every write path fails
    let (s, _) = t
        .req(
            "PATCH",
            &format!("/api/nodes/releases/{k1}"),
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(s, 409);
    assert!(t
        .store
        .update_node("releases", &k1, &json!({"name": "x"}))
        .await
        .is_err());
    let (s, _) = t
        .req(
            "POST",
            "/api/edges/included_in",
            Some(json!({"from": "versions/x", "to": format!("releases/{k1}")})),
        )
        .await;
    assert_eq!(s, 400);

    // promotion must be ordered
    let skip = t
        .ok(
            "POST",
            "/api/environments/qa/promote",
            Some(json!({"release": k1})),
        )
        .await;
    assert_eq!(skip["promoted"], false);
    // dev → test → qa → prod (prod needs a human)
    for env in ["test", "qa"] {
        let p = t
            .ok(
                "POST",
                &format!("/api/environments/{env}/promote"),
                Some(json!({"release": k1})),
            )
            .await;
        assert_eq!(p["promoted"], true, "{env}: {}", p["gate"]);
    }
    let no_human = t
        .ok(
            "POST",
            "/api/environments/prod/promote",
            Some(json!({"release": k1, "approved_by": "human"})),
        )
        .await;
    assert_eq!(
        no_human["promoted"], false,
        "a caller-supplied string is not an approval"
    );
    assert!(no_human["gate"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["check"] == "human approval" && c["ok"] == false));
    // approvals are only issued to the app (browser origin), are one-time and release-bound
    let (s, _) = t
        .req(
            "POST",
            "/api/environments/prod/approvals",
            Some(json!({"release": k1})),
        )
        .await;
    assert_eq!(s, 403);
    let (_, appr) = t
        .ui(
            "POST",
            "/api/environments/prod/approvals",
            Some(json!({"release": k1})),
        )
        .await;
    let id = appr["approval"].as_str().unwrap().to_string();
    let p = t
        .ok(
            "POST",
            "/api/environments/prod/promote",
            Some(json!({"release": k1, "approval": id})),
        )
        .await;
    assert_eq!(p["promoted"], true, "{}", p["gate"]);
    assert!(p["gate"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["check"].as_str().unwrap().contains("smoke")));
    let envs = t.ok("GET", "/api/environments", None).await;
    for env in ["dev", "test", "qa", "prod"] {
        assert_eq!(running(&envs, env).as_deref(), Some(k1.as_str()), "{env}");
    }
    let n = t.store.aql("FOR env IN @@environments RETURN LENGTH(FOR e IN @@currently_running FILTER e._from == env._id RETURN 1)", json!({})).await.unwrap();
    assert!(
        n.iter().all(|c| c.as_u64() == Some(1)),
        "exactly one currently_running per environment: {n:?}"
    );

    // a change → r2 → diff lists exactly that symbol
    edit(&t, "verify_credentials", |b| format!("// r2\n{b}")).await;
    let r2 = t
        .ok("POST", "/api/releases", Some(json!({"name": "r2"})))
        .await;
    let k2 = r2["_key"].as_str().unwrap().to_string();
    let diff = t
        .ok("GET", &format!("/api/releases/{k1}/diff/{k2}"), None)
        .await;
    let changed: Vec<&str> = diff["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(changed, vec!["verify_credentials"]);
    assert_eq!(diff["changed"][0]["reasons"], json!(["version"]));
    assert!(
        diff["added"].as_array().unwrap().is_empty()
            && diff["removed"].as_array().unwrap().is_empty()
    );

    // promote r2 to test, run in test env uses the release (not dev working state)
    let p = t
        .ok(
            "POST",
            "/api/environments/test/promote",
            Some(json!({"release": k2})),
        )
        .await;
    assert_eq!(p["promoted"], true, "{}", p["gate"]);
    let run = t
        .ok(
            "POST",
            "/api/run",
            Some(
                json!({"symbol": "verify_credentials", "environment": "test",
        "args": {"store": {"users": {}}, "username": "x", "password": "y"}}),
            ),
        )
        .await;
    assert_eq!(run["release"], json!(k2));
    assert_eq!(run["result"], "invalid_credentials");

    // rollback restores the previous release; still exactly one currently_running
    let rb = t.uok("POST", "/api/environments/test/rollback", None).await;
    assert_eq!(rb["rolled_back_to"], json!(k1));
    let envs = t.ok("GET", "/api/environments", None).await;
    assert_eq!(running(&envs, "test").as_deref(), Some(k1.as_str()));
    assert_eq!(t.ok("GET", "/api/invariants", None).await["ok"], true);
    // a second rollback walks further back (here: nothing older), never forward again
    let (s, _) = t.ui("POST", "/api/environments/test/rollback", None).await;
    assert_eq!(s, 400, "nothing earlier than r1 in test");
    let envs = t.ok("GET", "/api/environments", None).await;
    assert_eq!(running(&envs, "test").as_deref(), Some(k1.as_str()));

    // bisect: r3 breaks verify_password; first bad is r3
    edit(&t, "verify_password", |_| {
        "let _ = (password, hash);\nfalse".into()
    })
    .await;
    let r3 = t
        .ok("POST", "/api/releases", Some(json!({"name": "r3 broken"})))
        .await;
    let k3 = r3["_key"].as_str().unwrap().to_string();
    let b = t
        .ok(
            "POST",
            "/api/releases/bisect",
            Some(json!({"good": k1, "bad": k3})),
        )
        .await;
    assert_eq!(b["first_bad"], json!(k3));
    assert!(b["diff"]["changed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["symbol"] == "verify_password"));
    // the gate refuses to promote a broken release to qa
    t.ok(
        "POST",
        "/api/environments/test/promote",
        Some(json!({"release": k3})),
    )
    .await;
    let q = t
        .ok(
            "POST",
            "/api/environments/qa/promote",
            Some(json!({"release": k3})),
        )
        .await;
    assert_eq!(q["promoted"], false);
    t.stop().await;
}
