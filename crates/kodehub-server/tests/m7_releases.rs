//! M7 + S4 acceptance: releases select versions by rule, are open in DEV and immutable once cut,
//! move through DEV → TEST → QA → PRE-PROD → PROD one environment at a time, can be cloned into
//! their own or a lower environment, get patch releases for bugs found after DEV, roll back
//! through the gate, and still diff and bisect.
mod common;

use common::*;
use serde_json::{json, Value};

async fn edit(t: &TestServer, symbol: &str, f: impl Fn(&str) -> String) -> String {
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
        .unwrap()
        .version
}

fn in_env(envs: &Value, env: &str) -> Vec<String> {
    envs.as_array()
        .unwrap()
        .iter()
        .find(|e| e["_key"] == env)
        .map(|e| {
            e["releases"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["release"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn version_of(rel: &Value, symbol: &str) -> String {
    rel["manifest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["symbol"] == symbol)
        .map(|m| m["version"].as_str().unwrap().to_string())
        .unwrap_or_default()
}

async fn approval(t: &TestServer, env: &str, rel: &str) -> String {
    let (_, a) = t
        .ui(
            "POST",
            &format!("/api/environments/{env}/approvals"),
            Some(json!({"release": rel})),
        )
        .await;
    a["approval"].as_str().unwrap().to_string()
}

async fn promote(t: &TestServer, env: &str, rel: &str, approval: Option<String>) -> Value {
    t.ok(
        "POST",
        &format!("/api/environments/{env}/promote"),
        Some(json!({"release": rel, "approval": approval})),
    )
    .await
}

#[tokio::test]
async fn releases_select_move_clone_patch_rollback_diff_bisect() {
    let t = TestServer::start("rel").await;
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    t.ok("POST", "/api/tests/run", None).await;
    let before_edit = kodehub_core::model::now_iso();

    // create → open, in dev, supports every stable version at every level
    let r1 = t
        .ok(
            "POST",
            "/api/releases",
            Some(json!({"name": "first", "notes": "first"})),
        )
        .await;
    let k1 = r1["_key"].as_str().unwrap().to_string();
    assert_eq!(k1, "r1");
    assert_eq!(r1["state"], "open");
    assert!(r1["manifest"].as_array().unwrap().len() > 10);
    let sel = t
        .ok("GET", &format!("/api/releases/{k1}/selection"), None)
        .await;
    let sel = sel.as_array().unwrap();
    assert!(sel
        .iter()
        .any(|s| s["node"] == "symbols/verify_credentials" && s["collection"] == "versions"));
    assert!(sel
        .iter()
        .any(|s| s["node"] == "components/password-hashing" && s["collection"] == "c4_versions"));
    let envs = t.ok("GET", "/api/environments", None).await;
    let names: Vec<&str> = envs
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["_key"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["dev", "test", "qa", "preprod", "prod"]);
    assert_eq!(in_env(&envs, "dev"), vec![k1.clone()]);

    // releases are written only through release operations
    let (s, _) = t
        .req(
            "PATCH",
            &format!("/api/nodes/releases/{k1}"),
            Some(json!({"name": "x"})),
        )
        .await;
    assert!((400..500).contains(&s), "refused: {s}");
    let (s, _) = t
        .req(
            "POST",
            "/api/edges/supports",
            Some(json!({"from": "versions/x", "to": format!("releases/{k1}")})),
        )
        .await;
    assert_eq!(s, 400);
    // an open release can be reselected
    t.ok("POST", &format!("/api/releases/{k1}/reselect"), None)
        .await;

    // promotion is ordered, one environment at a time; leaving DEV cuts it
    let skip = promote(&t, "qa", &k1, None).await;
    assert_eq!(skip["promoted"], false);
    for env in ["test", "qa", "preprod"] {
        let p = promote(&t, env, &k1, None).await;
        assert_eq!(p["promoted"], true, "{env}: {}", p["gate"]);
    }
    let r = t.ok("GET", &format!("/api/releases/{k1}"), None).await;
    assert_eq!(r["state"], "cut");
    assert_eq!(r["environment"], "preprod");
    let (s, _) = t
        .req("POST", &format!("/api/releases/{k1}/reselect"), None)
        .await;
    assert_eq!(s, 409, "a cut release is immutable");
    // prod needs a human: a caller string is not an approval; agents cannot get one
    let p = t
        .ok(
            "POST",
            "/api/environments/prod/promote",
            Some(json!({"release": k1, "approved_by": "human"})),
        )
        .await;
    assert_eq!(p["promoted"], false);
    let (s, _) = t
        .req(
            "POST",
            "/api/environments/prod/approvals",
            Some(json!({"release": k1})),
        )
        .await;
    assert_eq!(s, 403);
    let p = promote(&t, "prod", &k1, Some(approval(&t, "prod", &k1).await)).await;
    assert_eq!(p["promoted"], true, "{}", p["gate"]);
    let envs = t.ok("GET", "/api/environments", None).await;
    assert_eq!(in_env(&envs, "prod"), vec![k1.clone()]);
    assert!(
        in_env(&envs, "preprod").is_empty(),
        "a release is in one environment at a time"
    );

    // a change → r2; several releases can sit in DEV at once; diff lists exactly that symbol
    let old_vc = version_of(&r1, "verify_credentials");
    edit(&t, "verify_credentials", |b| format!("// r2\n{b}")).await;
    let r2 = t
        .ok("POST", "/api/releases", Some(json!({"name": "second"})))
        .await;
    let k2 = r2["_key"].as_str().unwrap().to_string();
    let r3 = t
        .ok("POST", "/api/releases", Some(json!({"name": "third"})))
        .await;
    let k3 = r3["_key"].as_str().unwrap().to_string();
    let envs = t.ok("GET", "/api/environments", None).await;
    assert_eq!(in_env(&envs, "dev").len(), 2);
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
    assert!(
        diff["added"].as_array().unwrap().is_empty()
            && diff["removed"].as_array().unwrap().is_empty()
    );

    // as_of selection: the code as it was before the edit
    let r_asof = t
        .ok(
            "POST",
            "/api/releases",
            Some(json!({"name": "as of", "rule": {"mode": "as_of", "at": before_edit}})),
        )
        .await;
    assert_eq!(version_of(&r_asof, "verify_credentials"), old_vc);

    // tag filter (with build closure): only tagged code and what it needs
    t.ok(
        "PATCH",
        "/api/nodes/symbols/hash_password",
        Some(json!({"tags": ["crypto"]})),
    )
    .await;
    let r_tag = t
        .ok("POST", "/api/releases", Some(json!({"name": "crypto only", "rule": {"mode": "stable", "filter": {"tags_any": ["crypto"], "levels": ["code"]}}})))
        .await;
    let syms: Vec<&str> = r_tag["manifest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["symbol"].as_str().unwrap())
        .collect();
    assert!(syms.contains(&"hash_password"));
    assert!(!syms.contains(&"verify_credentials"));
    assert!(
        r_tag["c4_manifest"].as_array().unwrap().is_empty(),
        "levels filter keeps code only"
    );

    // r2 to test: run in test uses the release, not the working graph
    let p = promote(&t, "test", &k2, None).await;
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

    // r2 all the way to prod: the single-slot PROD retires r1
    for env in ["qa", "preprod"] {
        let p = promote(&t, env, &k2, None).await;
        assert_eq!(p["promoted"], true, "{env}: {}", p["gate"]);
    }
    let p = promote(&t, "prod", &k2, Some(approval(&t, "prod", &k2).await)).await;
    assert_eq!(p["promoted"], true, "{}", p["gate"]);
    let envs = t.ok("GET", "/api/environments", None).await;
    assert_eq!(in_env(&envs, "prod"), vec![k2.clone()]);
    assert_eq!(
        t.ok("GET", &format!("/api/releases/{k1}"), None).await["state"],
        "retired"
    );
    assert_eq!(t.ok("GET", "/api/invariants", None).await["ok"], true);

    // rollback: r1 re-enters prod through the gate (needs a human again)
    let target = t.store.rollback_target("prod").await.unwrap();
    assert_eq!(target.as_deref(), Some(k1.as_str()));
    let rb = t
        .uok(
            "POST",
            "/api/environments/prod/rollback",
            Some(json!({"approval": approval(&t, "prod", &k1).await})),
        )
        .await;
    assert_eq!(rb["rolled_back_to"], json!(k1), "{rb}");
    let envs = t.ok("GET", "/api/environments", None).await;
    assert_eq!(in_env(&envs, "prod"), vec![k1.clone()]);
    assert_eq!(
        t.ok("GET", &format!("/api/releases/{k2}"), None).await["state"],
        "retired"
    );

    // clone: same or lower environment only
    let c = t
        .ok(
            "POST",
            &format!("/api/releases/{k1}/clone"),
            Some(json!({"environment": "dev"})),
        )
        .await;
    assert_eq!(c["state"], "open");
    assert_eq!(c["cloned_from"], json!(k1));
    let ck = c["_key"].as_str().unwrap().to_string();
    let (s, _) = t
        .req(
            "POST",
            &format!("/api/releases/{ck}/clone"),
            Some(json!({"environment": "qa"})),
        )
        .await;
    assert_eq!(s, 400, "never upward");

    // patch release: a bug found in PROD's r1 is fixed and only the fix moves up
    let manifest_before =
        t.ok("GET", &format!("/api/releases/{k1}"), None).await["manifest_hash"].clone();
    let fixed = edit(&t, "verify_password", |b| format!("// fix\n{b}")).await;
    let (_, patched) = t
        .ui(
            "POST",
            &format!("/api/releases/{k1}/patch"),
            Some(json!({
                "swaps": [{"symbol": "verify_password", "version": fixed}],
                "approval": approval(&t, "prod", &k1).await,
            })),
        )
        .await;
    assert_eq!(patched["patched"], true, "{patched}");
    assert_eq!(patched["release"], json!(format!("{k1}.1")));
    let envs = t.ok("GET", "/api/environments", None).await;
    assert_eq!(in_env(&envs, "prod"), vec![format!("{k1}.1")]);
    let orig = t.ok("GET", &format!("/api/releases/{k1}"), None).await;
    assert_eq!(orig["state"], "retired");
    assert_eq!(
        orig["manifest_hash"], manifest_before,
        "the original is unchanged"
    );
    let pd = t
        .ok("GET", &format!("/api/releases/{k1}/diff/{k1}.1"), None)
        .await;
    let changed: Vec<&str> = pd["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(
        changed,
        vec!["verify_password"],
        "exactly one version swapped"
    );
    assert_eq!(t.ok("GET", "/api/invariants", None).await["ok"], true);

    // bisect: a later release breaks verify_password; first bad is that release
    edit(&t, "verify_password", |_| {
        "let _ = (password, hash);\nfalse".into()
    })
    .await;
    let r_bad = t
        .ok("POST", "/api/releases", Some(json!({"name": "broken"})))
        .await;
    let kb = r_bad["_key"].as_str().unwrap().to_string();
    let b = t
        .ok(
            "POST",
            "/api/releases/bisect",
            Some(json!({"good": k3, "bad": kb})),
        )
        .await;
    assert_eq!(b["first_bad"], json!(kb));
    // the gate refuses to move a broken release past test
    promote(&t, "test", &kb, None).await;
    let q = promote(&t, "qa", &kb, None).await;
    assert_eq!(q["promoted"], false);
    t.stop().await;
}
