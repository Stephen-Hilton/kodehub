//! M3 acceptance: tests + traceability on seed data.
mod common;

use common::*;
use kodehub_core::model::{Kind, Language, VersionContent};
use kodehub_runner::TestScope;
use kodehub_store::NewSymbol;
use serde_json::json;

fn status_of(cov: &kodehub_store::queries::Coverage, key: &str) -> String {
    cov.requirements
        .iter()
        .find(|r| r.key == key)
        .map(|r| r.status.clone())
        .or_else(|| {
            cov.groups
                .iter()
                .find(|g| g.key == key)
                .map(|g| g.status.clone())
        })
        .unwrap_or_else(|| panic!("no coverage row for {key}"))
}

#[tokio::test]
async fn coverage_passes_then_broken_helper_turns_requirements_red() {
    let s = seeded("cov").await;
    let r = runner();
    let g = s.load_code_graph().await.unwrap();

    // before running anything: tests exist but have no results → at risk
    let cov = s.coverage(None).await.unwrap();
    assert_eq!(cov.requirements.len(), 2);
    assert_eq!(status_of(&cov, "r1-generic-failure"), "at_risk");

    let tests = r.resolve_scope(&s, &g, &TestScope::All).await.unwrap();
    assert_eq!(tests.len(), 9);
    let out = r
        .run_tests(&s, &g, &tests, false, None, None)
        .await
        .unwrap();
    let failed: Vec<_> = out.iter().filter(|o| o.status != "pass").collect();
    assert!(failed.is_empty(), "failing tests: {failed:#?}");

    let cov = s.coverage(None).await.unwrap();
    for req in &cov.requirements {
        assert_eq!(
            req.status, "pass",
            "{} not passing: {:?}",
            req.key, req.tests
        );
        assert!(!req.tests.is_empty());
    }
    assert_eq!(status_of(&cov, "secure-authentication"), "pass");

    // cached passing results are not re-run
    let again = r
        .run_tests(&s, &g, &tests, false, None, None)
        .await
        .unwrap();
    assert!(again.iter().all(|o| o.cached));

    // break a helper: verify_password always returns false
    edit_body(&s, "verify_password", |_| {
        "let _ = (password, hash);\nfalse".into()
    })
    .await;
    let g2 = s.load_code_graph().await.unwrap();
    let affected = r
        .resolve_scope(&s, &g2, &TestScope::Symbol("verify_password".into()))
        .await
        .unwrap();
    assert!(affected.contains(&"test_valid_login_succeeds".to_string()));
    assert!(
        !affected.iter().any(|t| t.starts_with("py-")),
        "python tests don't depend on the rust helper"
    );
    let out = r
        .run_tests(&s, &g2, &affected, false, None, None)
        .await
        .unwrap();
    assert!(out
        .iter()
        .any(|o| o.status == "fail" && o.name == "test_valid_login_succeeds"));
    let cov = s.coverage(None).await.unwrap();
    assert_eq!(status_of(&cov, "r1-generic-failure"), "fail");
    assert_eq!(status_of(&cov, "r2-hashed-storage"), "fail");
    assert_eq!(status_of(&cov, "secure-authentication"), "fail");

    // node health propagates red up the C4 hierarchy
    let health = s.node_health().await.unwrap();
    assert_eq!(
        health["symbols/verify_credentials"]["health"],
        json!("fail")
    );
    assert_eq!(
        health["components/password-hashing"]["health"],
        json!("fail")
    );
    assert_eq!(health["contexts/authentication"]["health"], json!("fail"));
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn orphans_impact_and_duplicates() {
    let s = seeded("trace").await;
    assert!(s.orphans().await.unwrap().is_empty(), "seed has no orphans");

    s.create_symbol(
        &NewSymbol {
            key: Some("lonely".into()),
            display_name: "lonely".into(),
            kind: Kind::Function,
            language: Language::Rust,
            visibility: Default::default(),
            tags: vec![],
            description: None,
        },
        &VersionContent {
            kind: Kind::Function,
            language: Language::Rust,
            signature: "() -> i32".into(),
            body: "42".into(),
            attributes: json!({}),
        },
        "test",
    )
    .await
    .unwrap();
    let orphans = s.orphans().await.unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0]["key"], json!("lonely"));

    let imp = s.impact("hash_password").await.unwrap();
    let groups: Vec<&str> = imp["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    let reqs: Vec<&str> = imp["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert_eq!(groups, vec!["requirement_groups/secure-authentication"]);
    assert_eq!(
        reqs,
        vec![
            "requirements/r1-generic-failure",
            "requirements/r2-hashed-storage"
        ]
    );

    let dups = s.duplicates().await.unwrap();
    assert_eq!(dups.len(), 1);
    let names: Vec<&str> = dups[0]["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["display_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["check_password", "verify_password"]);

    // near duplicates: a slightly edited copy of check_password is flagged, unrelated code is not
    edit_body(&s, "check_password", |b| {
        format!(
            "// tolerant variant\n{}",
            b.replace("Err(_) => false", "Err(_) => { false }")
        )
    })
    .await;
    assert!(s.duplicates().await.unwrap().is_empty());
    let near = s.near_duplicates(0.6).await.unwrap();
    assert_eq!(near.len(), 1, "{near:?}");
    assert!(near[0]["similarity"].as_f64().unwrap() > 0.8);
    s.drop_db().await.unwrap();
}
