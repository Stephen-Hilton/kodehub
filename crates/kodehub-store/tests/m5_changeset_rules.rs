//! Changeset merge rules from the M6 critical review (#1 coverage of requirement edits, #4
//! attested reviews, #2 state machine).
use kodehub_core::model::ChangesetState;
use kodehub_store::changesets::ProposeVersion;
use kodehub_store::seed::import_dir;
use kodehub_store::testutil::{seed_dir, temp_store};
use serde_json::json;

#[tokio::test]
async fn requirement_edits_need_a_verifying_test_and_an_attested_review() {
    let s = temp_store("csrules").await.unwrap();
    import_dir(&s, &seed_dir()).await.unwrap();
    s.update_requirement("r1-generic-failure", &json!({"edge_cases": [{"description": "locked", "input": {}, "expected": "invalid_credentials"}]}), "human", true).await.unwrap();
    let cs = s
        .open_changeset(
            "Address R1",
            "edge case",
            &["r1-generic-failure".into()],
            "orchestrator",
        )
        .await
        .unwrap();
    let k = cs["_key"].as_str().unwrap().to_string();
    assert_eq!(cs["addresses"]["r1-generic-failure"], 2);
    s.patch_changeset(&k, json!({"origin": "requirement_edit"}))
        .await
        .unwrap();
    s.set_plan(&k, json!({"summary": "x", "changes": []}), "orchestrator")
        .await
        .unwrap();
    s.transition(&k, ChangesetState::Approved, "human", "")
        .await
        .unwrap();
    s.transition(&k, ChangesetState::Building, "orchestrator", "")
        .await
        .unwrap();
    let cur = s.aql("FOR e IN @@current_version FILTER e._to == CONCAT(@pfx, 'symbols/verify_credentials') RETURN DOCUMENT(e._from)", json!({})).await.unwrap();
    s.propose_version(
        &k,
        &ProposeVersion {
            symbol: Some("verify_credentials".into()),
            signature: cur[0]["signature"].as_str().unwrap().into(),
            body: format!("// no-op\n{}", cur[0]["body"].as_str().unwrap()),
            by: Some("coder".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // a review over the API is advisory
    s.submit_review(&k, &[], "lgtm", "someone", "api")
        .await
        .unwrap();
    let (c, _, _) = s.merge_checks(&k).await.unwrap();
    assert!(!c.review_attested && !c.review_ok);
    // the engine's reviewer attests, but no test verifies the edited requirement yet
    s.submit_review(&k, &[], "ok", "reviewer", "engine_reviewer")
        .await
        .unwrap();
    let (c, _, _) = s.merge_checks(&k).await.unwrap();
    assert!(c.review_ok);
    assert!(!c.requirements_covered);
    assert_eq!(
        c.uncovered_requirements,
        vec!["r1-generic-failure".to_string()]
    );
    // a proposal changes the fingerprint → the earlier review no longer counts
    s.propose_version(
        &k,
        &ProposeVersion {
            display_name: Some("test_locked".into()),
            kind: Some(kodehub_core::model::Kind::Test),
            language: Some(kodehub_core::model::Language::Rust),
            signature: String::new(),
            body: "assert!(true);".into(),
            verifies: vec!["r1-generic-failure".into()],
            by: Some("tester".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (c, _, _) = s.merge_checks(&k).await.unwrap();
    assert!(c.requirements_covered);
    assert!(!c.review_ok, "review must be of exactly what merges");
    // blocking findings from an earlier round must be resolved, not merely absent later
    s.submit_review(
        &k,
        &[json!({"severity": "blocking", "location": "x", "finding": "bad"})],
        "no",
        "reviewer",
        "engine_reviewer",
    )
    .await
    .unwrap();
    s.submit_review(&k, &[], "ok now?", "reviewer", "engine_reviewer")
        .await
        .unwrap();
    let (c, _, _) = s.merge_checks(&k).await.unwrap();
    assert!(!c.review_ok && c.open_blocking.len() == 1);
    // merged is never a plain transition from anywhere but ready
    assert!(s
        .transition(&k, ChangesetState::Merged, "x", "")
        .await
        .is_err());
    s.drop_db().await.unwrap();
}
