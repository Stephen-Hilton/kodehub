//! S3 acceptance: SCD versions at every C4 level, candidates, requirement kinds + inheritance +
//! counts, philosophy, use cases, lifecycle roll-up, the blind_review setting.
mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn versioned_c4_model_requirements_use_cases_lifecycle() {
    let t = TestServer::start("s3c4").await;
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    assert!(t.ok("GET", "/api/invariants", None).await["problems"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(true));

    // philosophy replaces the mission, and is the project root
    let ph = t.ok("GET", "/api/philosophy", None).await;
    assert!(ph["text"].as_str().unwrap().contains("never leaks"));
    let map = t.ok("GET", "/api/map", None).await;
    assert!(map["mission"]["statement"]
        .as_str()
        .unwrap()
        .contains("never leaks"));

    // contexts / containers / components have versions; editing one writes a new version into
    // the current project version (the old one stays, held by no current tree)
    let v1 = t
        .ok(
            "GET",
            "/api/node-versions?node=components/password-hashing",
            None,
        )
        .await;
    assert_eq!(v1.as_array().unwrap().len(), 1);
    assert_eq!(v1[0]["state"], "accepted");
    assert_eq!(v1[0]["current"], true);
    assert_eq!(v1[0]["collection"], "component_versions");
    assert_eq!(v1[0]["project_versions"].as_array().unwrap().len(), 1);
    t.ok(
        "PATCH",
        "/api/nodes/components/password-hashing",
        Some(json!({"description": "Argon2id hashing, verification and rehash."})),
    )
    .await;
    let v2 = t
        .ok(
            "GET",
            "/api/node-versions?node=components/password-hashing",
            None,
        )
        .await;
    assert_eq!(v2.as_array().unwrap().len(), 2);
    assert_eq!(v2[0]["current"], true);
    assert_eq!(v2[0]["seq"], 2);
    assert_eq!(v2[1]["current"], false);
    assert!(v2[1]["project_versions"].as_array().unwrap().is_empty());
    // its container's and context's versions follow (they list their children)
    let cv = t
        .ok(
            "GET",
            "/api/node-versions?node=containers/auth-service",
            None,
        )
        .await;
    assert_eq!(cv.as_array().unwrap().len(), 2);
    // a no-op patch creates no version
    t.ok(
        "PATCH",
        "/api/nodes/components/password-hashing",
        Some(json!({"description": "Argon2id hashing, verification and rehash."})),
    )
    .await;
    assert_eq!(
        t.ok(
            "GET",
            "/api/node-versions?node=components/password-hashing",
            None
        )
        .await
        .as_array()
        .unwrap()
        .len(),
        2
    );

    // requirement kinds, by kind and by level
    let biz = t
        .ok("GET", "/api/requirements-by?kind=business", None)
        .await;
    assert_eq!(biz.as_array().unwrap().len(), 1);
    assert_eq!(biz[0]["key"], t.q("r1-generic-failure"));
    let ctx = t
        .ok("GET", "/api/requirements-by?level=context", None)
        .await;
    assert_eq!(
        ctx.as_array().unwrap().len(),
        2,
        "both apply to the context"
    );

    // inheritance: verify_credentials has R1 itself and inherits R2 from its context
    let c = t
        .ok("GET", "/api/cascade?node=code/verify_credentials", None)
        .await;
    let reqs = c["requirements"].as_array().unwrap();
    let r1 = reqs
        .iter()
        .find(|r| r["key"] == t.q("r1-generic-failure"))
        .unwrap();
    assert_eq!(r1["inherited"], false);
    let r2 = reqs
        .iter()
        .find(|r| r["key"] == t.q("r2-hashed-storage"))
        .unwrap();
    assert_eq!(r2["inherited"], true);
    assert_eq!(r2["attached_level"], "context");
    assert_eq!(
        c["chain"].as_array().unwrap().last().unwrap(),
        &t.store.project_node().unwrap()
    );
    assert!(c["philosophy"].as_str().unwrap().contains("never leaks"));

    // per-node counts: own vs with children
    let counts = t.ok("GET", "/api/requirement-counts", None).await;
    let comp = &counts[&t.q("components/credential-verification")];
    assert_eq!(comp["own"]["business"], 0);
    assert_eq!(
        comp["total"]["business"], 1,
        "R1 on verify_credentials below it"
    );
    let ctxc = &counts[&t.q("contexts/authentication")];
    assert_eq!(ctxc["own"]["technical"], 1);
    assert_eq!(ctxc["own"]["business"], 1);

    // lifecycle before any test run: built code with tests is ready for test, not live
    let lc = t.ok("GET", "/api/lifecycle", None).await;
    assert_eq!(
        lc[&t.q("code/verify_credentials")]["state"],
        "ready_for_test"
    );
    t.ok(
        "POST",
        "/api/tests/run",
        Some(json!({"scope": {"kind": "all"}})),
    )
    .await;
    let lc = t.ok("GET", "/api/lifecycle", None).await;
    assert_eq!(lc[&t.q("code/verify_credentials")]["state"], "live");
    assert_eq!(
        lc[&t.q("components/credential-verification")]["state"],
        "live",
        "all children live → live"
    );
    // the seed's untested duplicate helper has no tests, so it is not ready for test (§14.4),
    // and it holds its component, container and context back
    assert_eq!(lc[&t.q("code/check_password")]["state"], "in_build");
    assert_eq!(lc[&t.q("components/password-hashing")]["state"], "in_build");
    assert_eq!(lc[&t.q("contexts/authentication")]["state"], "in_build");
    // a planned child keeps its parent from being live; rework flags in_build
    t.ok(
        "POST",
        "/api/nodes/components",
        Some(json!({"_key": "rate-limiting", "title": "rate-limiting"})),
    )
    .await;
    t.ok(
        "POST",
        "/api/edges/contains",
        Some(json!({"from": "containers/auth-service", "to": "components/rate-limiting"})),
    )
    .await;
    let lc = t.ok("GET", "/api/lifecycle", None).await;
    assert_eq!(lc[&t.q("components/rate-limiting")]["state"], "planned");
    assert_eq!(lc[&t.q("containers/auth-service")]["state"], "in_build");
    let map = t.ok("GET", "/api/map", None).await;
    let node = map["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == t.q("containers/auth-service"))
        .unwrap();
    assert_eq!(node["lifecycle"], "in_build");
    assert!(node["requirements"]["total"]["technical"].as_u64().unwrap() >= 1);

    // candidate versions never disturb the accepted one
    let cs = t
        .ok(
            "POST",
            "/api/proposals",
            Some(json!({"title": "tweak", "intent": "x", "requirements": []})),
        )
        .await;
    let k = cs["_key"].as_str().unwrap().to_string();
    let before = t
        .ok("GET", "/api/node-versions?node=code/hash_password", None)
        .await;
    let sym = t.ok("GET", "/api/symbols/hash_password", None).await;
    let sig = sym["version"]["signature"]
        .as_str()
        .unwrap_or("(password: &str) -> String")
        .to_string();
    t.ok(
        "POST",
        &format!("/api/work-items/{k}/proposal/versions"),
        Some(json!({
            "symbol": "hash_password", "signature": sig, "body": "// candidate\ntodo!()",
        })),
    )
    .await;
    let after = t
        .ok("GET", "/api/node-versions?node=code/hash_password", None)
        .await;
    assert_eq!(
        after.as_array().unwrap().len(),
        before.as_array().unwrap().len() + 1
    );
    assert_eq!(after[0]["state"], "candidate");
    assert_eq!(after[0]["work_item"], json!(k));
    assert_eq!(after[0]["current"], false);
    assert_eq!(after[1]["state"], "accepted");
    assert_eq!(after[1]["current"], true);
    let lc = t.ok("GET", "/api/lifecycle", None).await;
    assert_eq!(lc[&t.q("code/hash_password")]["state"], "in_build");
    // blind review: required by default; off → the gate no longer needs it
    let checks = t
        .ok("GET", &format!("/api/work-items/{k}/proposal/checks"), None)
        .await["checks"]
        .clone();
    assert_eq!(checks["review_required"], true);
    assert_eq!(checks["review_ok"], false);
    let pid = t.store.project_id().unwrap();
    t.uok(
        "PUT",
        &format!("/api/scopes/project:{pid}/settings/blind_review"),
        Some(json!({"value": false})),
    )
    .await;
    let checks = t
        .ok("GET", &format!("/api/work-items/{k}/proposal/checks"), None)
        .await["checks"]
        .clone();
    assert_eq!(checks["review_required"], false);
    assert_eq!(checks["review_ok"], true);
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/reject"),
        Some(json!({"by": "human", "reason": "test"})),
    )
    .await;
    let after = t
        .ok("GET", "/api/node-versions?node=code/hash_password", None)
        .await;
    assert_eq!(after[0]["state"], "discarded");

    // use cases: draft → live needs an expected-outcome test
    let uc = t
        .ok(
            "POST",
            "/api/use-cases",
            Some(json!({
                "name": "A user signs in", "description": "Valid credentials sign the user in.",
                "requirements": ["r1-generic-failure"],
            })),
        )
        .await;
    assert_eq!(uc["state"], "draft");
    assert_eq!(uc["requirements"][0]["key"], t.q("r1-generic-failure"));
    let (s, _) = t
        .ui(
            "POST",
            "/api/use-cases/a-user-signs-in/state",
            Some(json!({"state": "live"})),
        )
        .await;
    assert_eq!(s, 400);
    t.ok(
        "POST",
        "/api/use-cases/a-user-signs-in/links",
        Some(json!({"edge": "verified_by", "to": "tests/test_valid_login_succeeds"})),
    )
    .await;
    let uc = t
        .uok(
            "POST",
            "/api/use-cases/a-user-signs-in/state",
            Some(json!({"state": "live"})),
        )
        .await;
    // going live first passes an architect's contradiction check (spec §15.3)
    assert_eq!(uc["state"], "checking");
    let items = t.ok("GET", "/api/work-items", None).await;
    assert!(items
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["agent_type"] == "architect" && w["kind"] == "check_use_case"));
    let uc = t
        .store
        .use_case_verdict(
            "a-user-signs-in",
            true,
            json!([]),
            "no conflicts",
            "architect",
        )
        .await
        .unwrap();
    assert_eq!(uc["state"], "live");
    assert_eq!(
        t.ok("GET", "/api/use-cases", None)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(t.ok("GET", "/api/invariants", None).await["problems"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(true));
    t.stop().await;
}
