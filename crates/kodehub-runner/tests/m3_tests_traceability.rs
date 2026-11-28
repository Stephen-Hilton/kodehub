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
        .find(|r| kodehub_store::arango::local_key(&r.key) == key)
        .map(|r| r.status.clone())
        .or_else(|| {
            cov.tags
                .iter()
                .find(|g| g.tag == key)
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
        .resolve_scope(&s, &g2, &TestScope::Symbol(q(&s, "verify_password")))
        .await
        .unwrap();
    assert!(affected.contains(&q(&s, "test_valid_login_succeeds")));
    assert!(
        !affected
            .iter()
            .any(|t| kodehub_store::arango::local_key(t).starts_with("py-")),
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
        health[&q(&s, "code/verify_credentials")]["health"],
        json!("fail")
    );
    assert_eq!(
        health[&q(&s, "components/password-hashing")]["health"],
        json!("fail")
    );
    assert_eq!(
        health[&q(&s, "contexts/authentication")]["health"],
        json!("fail")
    );
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn orphans_impact_and_duplicates() {
    let s = seeded("trace").await;
    assert!(s.orphans().await.unwrap().is_empty(), "seed has no orphans");

    // a requirement covers everything beneath what it applies to, so an orphan needs a home
    // no requirement reaches: a new context
    s.insert_node("contexts", json!({"_key": "scratch", "title": "Scratch"}))
        .await
        .unwrap();
    s.insert_edge(
        "contains",
        &s.project_node().unwrap(),
        "contexts/scratch",
        json!({}),
    )
    .await
    .unwrap();
    s.create_symbol(
        &NewSymbol {
            key: Some("lonely".into()),
            display_name: "lonely".into(),
            kind: Kind::Function,
            language: Language::Rust,
            visibility: Default::default(),
            tags: vec![],
            description: None,
            parent: Some("contexts/scratch".into()),
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
    assert_eq!(orphans[0]["key"], json!(q(&s, "lonely")));

    let imp = s.impact("hash_password").await.unwrap();
    let tags: Vec<&str> = imp["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g.as_str().unwrap())
        .collect();
    let reqs: Vec<&str> = imp["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].as_str().unwrap())
        .collect();
    assert_eq!(tags, vec!["secure-authentication"]);
    assert_eq!(
        reqs,
        vec![
            q(&s, "requirements/r1-generic-failure"),
            q(&s, "requirements/r2-hashed-storage")
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
