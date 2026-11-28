//! M7 + S4 acceptance, ontology v2 (D-059, D-060): project versions are the releases — open
//! while work goes in, frozen on the first deployment beyond DEV (or by hand). Deployments carry
//! one into one environment, one environment up at a time, gated; production waits for a
//! human. Hotfixes branch from a frozen version and go straight to the environment they fix.
//! Rollback goes through the gate; diff and bisect follow the version line.
mod common;

use common::*;
use serde_json::{json, Value};

/// A direct write into the current project version (as an accepted proposal would do).
async fn edit(t: &TestServer, symbol: &str, f: impl Fn(&str) -> String) -> String {
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
        .unwrap()
        .version
}

async fn deploy(t: &TestServer, pv: &str, env: &str) -> Value {
    t.uok(
        "POST",
        "/api/deployments",
        Some(json!({"project_version": pv, "environment": env})),
    )
    .await
}

/// Project versions running in an environment (succeeded, not superseded).
async fn running(t: &TestServer, env: &str) -> Vec<String> {
    let envs = t.ok("GET", "/api/environments", None).await;
    let e = envs
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == env)
        .unwrap_or_else(|| panic!("no environment {env}: {envs}"))
        .clone();
    e["running"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["project_version"].as_str().unwrap().to_string())
        .collect()
}

/// The key of a project version id (paths take keys).
fn key(id: &str) -> String {
    id.rsplit('/').next().unwrap().to_string()
}

async fn version(t: &TestServer, pv: &str) -> Value {
    let pv_k = key(pv);
    t.ok("GET", &format!("/api/project-versions/{pv_k}"), None)
        .await
}

async fn current(t: &TestServer) -> String {
    let all = t.ok("GET", "/api/project-versions", None).await;
    all.as_array()
        .unwrap()
        .iter()
        .find(|v| v["current"] == true)
        .map(|v| v["_id"].as_str().unwrap().to_string())
        .expect("a current project version")
}

fn changed_ids(diff: &Value) -> Vec<String> {
    diff["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn project_versions_deploy_freeze_hotfix_rollback_diff_bisect() {
    let t = TestServer::start("deploy").await;
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    t.ok("POST", "/api/tests/run", None).await;

    // one open project version, current; five environments, one deployer each
    let versions = t.ok("GET", "/api/project-versions", None).await;
    assert_eq!(versions.as_array().unwrap().len(), 1, "{versions}");
    let r1 = current(&t).await;
    let r1_k = key(&r1);
    assert_eq!(version(&t, &r1).await["state"], "open");
    let envs = t.ok("GET", "/api/environments", None).await;
    let names: Vec<&str> = envs
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["dev", "test", "qa", "preprod", "prod"]);
    for e in envs.as_array().unwrap() {
        assert_eq!(e["deployers"].as_array().unwrap().len(), 1, "{e}");
    }

    // a remote agent never deploys (D-061)
    let (s, _) = t
        .req(
            "POST",
            "/api/deployments",
            Some(json!({"project_version": r1, "environment": "dev"})),
        )
        .await;
    assert_eq!(s, 403);

    // DEV: deployed, the version stays open
    let d = deploy(&t, &r1, "dev").await;
    assert_eq!(d["deployed"], true, "{d}");
    assert_eq!(version(&t, &r1).await["state"], "open");
    assert_eq!(running(&t, "dev").await, vec![r1.clone()]);
    // ordered: not straight to QA
    let (s, e) = t
        .ui(
            "POST",
            "/api/deployments",
            Some(json!({"project_version": r1, "environment": "qa"})),
        )
        .await;
    assert_eq!(s, 400, "{e}");

    // TEST: r1 is cut (frozen) and r2 opens as current, with the same tree
    let d = deploy(&t, &r1, "test").await;
    assert_eq!(d["deployed"], true, "{}", d["gate"]);
    assert_eq!(version(&t, &r1).await["state"], "frozen");
    let r2 = current(&t).await;
    let r2_k = key(&r2);
    assert_ne!(r2, r1);
    assert_eq!(version(&t, &r2).await["based_on"], json!(r1));
    let diff = t
        .ok(
            "GET",
            &format!("/api/project-versions/{r1_k}/diff/{r2_k}"),
            None,
        )
        .await;
    assert!(changed_ids(&diff).is_empty(), "{diff}");
    // a frozen version refuses changes: proposals go into an open version
    let (s, _) = t
        .ui(
            "POST",
            "/api/proposals",
            Some(json!({"title": "x", "project_version": r1})),
        )
        .await;
    assert_eq!(s, 400, "a frozen version takes no work");

    // work goes on in r2 without touching r1
    edit(&t, "verify_credentials", |b| format!("// r2\n{b}")).await;
    let diff = t
        .ok(
            "GET",
            &format!("/api/project-versions/{r1_k}/diff/{r2_k}"),
            None,
        )
        .await;
    assert!(
        changed_ids(&diff).contains(&t.q("code/verify_credentials")),
        "{diff}"
    );
    // run in TEST uses the version deployed there, not the working tree
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
    assert_eq!(run["project_version"], json!(r1));
    assert_eq!(run["result"], "invalid_credentials");

    // QA, PRE-PROD through their gates
    for env in ["qa", "preprod"] {
        let d = deploy(&t, &r1, env).await;
        assert_eq!(d["deployed"], true, "{env}: {}", d["gate"]);
        let logs = d["deployment"]["logs"].as_array().unwrap();
        assert!(logs.iter().any(|l| l["step"] == "gate"), "{d}");
    }
    // PROD waits for a human; an agent token can't approve, a human in the app can
    let p = deploy(&t, &r1, "prod").await;
    assert_eq!(p["awaiting_approval"], true, "{p}");
    let pk = p["deployment"]["_key"].as_str().unwrap().to_string();
    let (s, _) = t
        .req("POST", &format!("/api/deployments/{pk}/approve"), None)
        .await;
    assert_eq!(s, 403);
    let a = t
        .uok("POST", &format!("/api/deployments/{pk}/approve"), None)
        .await;
    assert_eq!(a["deployed"], true, "{a}");
    assert_eq!(running(&t, "prod").await, vec![r1.clone()]);
    let v1 = version(&t, &r1).await;
    assert_eq!(v1["running_in"].as_array().unwrap().len(), 5, "{v1}");

    // r2 is frozen by hand (a milestone); r3 opens. r2 goes all the way to prod and
    // supersedes r1 there (same deploy target)
    t.uok(
        "POST",
        &format!("/api/project-versions/{r2_k}/freeze"),
        Some(json!({"reason": "milestone"})),
    )
    .await;
    assert_eq!(version(&t, &r2).await["state"], "frozen");
    let r3 = current(&t).await;
    let r3_k = key(&r3);
    assert_ne!(r3, r2);
    for env in ["dev", "test", "qa", "preprod"] {
        let d = deploy(&t, &r2, env).await;
        assert_eq!(d["deployed"], true, "{env}: {}", d["gate"]);
    }
    let p = deploy(&t, &r2, "prod").await;
    let pk = p["deployment"]["_key"].as_str().unwrap().to_string();
    t.uok("POST", &format!("/api/deployments/{pk}/approve"), None)
        .await;
    assert_eq!(running(&t, "prod").await, vec![r2.clone()]);
    assert_eq!(t.ok("GET", "/api/invariants", None).await["ok"], true);

    // rollback: r1 re-enters prod through the gate, and needs a human again
    let rb = t.uok("POST", "/api/environments/prod/rollback", None).await;
    assert_eq!(rb["rolled_back_to"], json!(r1), "{rb}");
    assert_eq!(rb["awaiting_approval"], true, "{rb}");
    let rk = rb["deployment"]["_key"].as_str().unwrap().to_string();
    t.uok("POST", &format!("/api/deployments/{rk}/approve"), None)
        .await;
    assert_eq!(running(&t, "prod").await, vec![r1.clone()]);

    // hotfix: a bug found in prod's r1 is fixed in a branch of r1, through a proposal, and only
    // the fix differs; r3 stays the current version
    let hf = t
        .uok(
            "POST",
            &format!("/api/project-versions/{r1_k}/hotfix"),
            Some(json!({"name": "r1-hotfix"})),
        )
        .await;
    let hf = hf["_id"].as_str().unwrap().to_string();
    let hf_k = key(&hf);
    assert_eq!(version(&t, &hf).await["state"], "open");
    assert_eq!(current(&t).await, r3, "the hotfix is not current");
    let wp = t
        .ok(
            "POST",
            "/api/proposals",
            Some(json!({"title": "Fix verify_password", "project_version": hf})),
        )
        .await;
    let k = wp["_key"].as_str().unwrap().to_string();
    t.ok("POST", &format!("/api/work-items/{k}/proposal/plan"), Some(json!({"plan": {"summary": "fix", "changes": [{"symbol": "verify_password", "action": "modify"}]}}))).await;
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/approve"),
        None,
    )
    .await;
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/transition"),
        Some(json!({"to": "building"})),
    )
    .await;
    let cur = t.ok("GET", "/api/symbols/verify_password", None).await;
    t.ok(
        "POST",
        &format!("/api/work-items/{k}/proposal/versions"),
        Some(
            json!({"symbol": "verify_password", "signature": cur["version"]["signature"],
                    "body": format!("// fix\n{}", cur["version"]["body"].as_str().unwrap()),
                    "by": "coder"}),
        ),
    )
    .await;
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/transition"),
        Some(json!({"to": "reviewing"})),
    )
    .await;
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/reviews"),
        Some(json!({"findings": [], "summary": "ok", "by": "human"})),
    )
    .await;
    let tr = t
        .ok("POST", &format!("/api/work-items/{k}/proposal/test"), None)
        .await;
    assert_eq!(tr["state"], "ready", "{tr}");
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/accept"),
        None,
    )
    .await;
    let diff = t
        .ok(
            "GET",
            &format!("/api/project-versions/{r1_k}/diff/{hf_k}"),
            None,
        )
        .await;
    let leaves: Vec<String> = changed_ids(&diff)
        .into_iter()
        .filter(|id| id.starts_with("code/") || id.starts_with("tests/"))
        .collect();
    assert_eq!(leaves, vec![t.q("code/verify_password")], "{diff}");
    let cur_after = t.ok("GET", "/api/symbols/verify_password", None).await;
    assert_eq!(
        cur_after["version"]["_key"], cur["version"]["_key"],
        "the current version r3 is untouched by the hotfix"
    );
    // straight to prod (not ordered), approved by a human; it supersedes r1 there
    let h = t
        .uok(
            "POST",
            "/api/deployments",
            Some(json!({"project_version": hf, "environment": "prod", "reason": "hotfix"})),
        )
        .await;
    assert_eq!(h["awaiting_approval"], true, "{h}");
    let hk = h["deployment"]["_key"].as_str().unwrap().to_string();
    t.uok("POST", &format!("/api/deployments/{hk}/approve"), None)
        .await;
    assert_eq!(running(&t, "prod").await, vec![hf.clone()]);
    assert_eq!(
        version(&t, &hf).await["state"],
        "frozen",
        "deployed beyond DEV"
    );
    assert!(
        running(&t, "qa").await.contains(&r2),
        "lower environments unchanged"
    );
    let deps = t.ok("GET", "/api/deployments?environment=prod", None).await;
    assert_eq!(deps.as_array().unwrap().len(), 4, "{deps}");

    // bisect along the version line: r3 breaks verify_password; r4 inherits it
    edit(&t, "verify_password", |_| {
        "let _ = (password, hash);\nfalse".into()
    })
    .await;
    t.uok(
        "POST",
        &format!("/api/project-versions/{r3_k}/freeze"),
        None,
    )
    .await;
    let r4 = current(&t).await;
    let b = t
        .ok(
            "POST",
            "/api/project-versions/bisect",
            Some(json!({"good": r2, "bad": r4})),
        )
        .await;
    assert_eq!(b["first_bad"], json!(r3), "{b}");
    assert_eq!(b["last_good"], json!(r2));
    let (s, _) = t
        .ui(
            "POST",
            "/api/project-versions/bisect",
            Some(json!({"good": hf, "bad": r4})),
        )
        .await;
    assert_eq!(s, 400, "a hotfix branch is not on r4's line");

    // the QA gate refuses the broken version; a retry is logged and fails again
    for env in ["dev", "test"] {
        let d = deploy(&t, &r3, env).await;
        assert_eq!(d["deployed"], true, "{env}: {}", d["gate"]);
    }
    let q = deploy(&t, &r3, "qa").await;
    assert_eq!(q["deployed"], false, "{q}");
    let qk = q["deployment"]["_key"].as_str().unwrap().to_string();
    assert_eq!(q["deployment"]["state"], "failed");
    let again = t
        .uok("POST", &format!("/api/deployments/{qk}/retry"), None)
        .await;
    assert_eq!(again["deployed"], false);
    let dq = t.ok("GET", &format!("/api/deployments/{qk}"), None).await;
    assert_eq!(dq["attempts"], 2, "{dq}");
    assert!(running(&t, "qa").await.contains(&r2), "qa still runs r2");

    // the time slider shows the frozen versions
    let tl = t.ok("GET", "/api/timeline", None).await;
    let frozen = tl["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "frozen")
        .count();
    assert_eq!(frozen, 4, "{tl}");
    // the map of an older version shows what changed since
    let m = t.ok("GET", &format!("/api/map?at={r1}"), None).await;
    assert_eq!(m["project_version"]["current"], false);
    assert!(m["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["id"] == t.q("code/verify_credentials") && n["changed_since"] == true));
    assert_eq!(t.ok("GET", "/api/invariants", None).await["ok"], true);
    t.stop().await;
}
