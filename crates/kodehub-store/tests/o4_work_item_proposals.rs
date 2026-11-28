//! Ontology step 4 (D-058): work items carry the proposal; acceptance moves the project
//! version's tree in one transaction.
use kodehub_core::model::{Kind, Language, ProposalState};
use kodehub_store::proposals::ProposeVersion;
use kodehub_store::queue::NewWorkItem;
use kodehub_store::seed::import_dir;
use kodehub_store::testutil::{seed_dir, temp_store};
use kodehub_store::{Store, StoreError};
use serde_json::json;

async fn work_item(s: &Store, target: &str) -> String {
    let p = s.project_id().unwrap();
    let w = s
        .enqueue(&NewWorkItem {
            project: p,
            agent_type: "coding".into(),
            kind: "implement".into(),
            target: Some(target.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    w["_key"].as_str().unwrap().to_string()
}

/// Draft → planned → approved → building.
async fn building(s: &Store, wi: &str, reqs: &[String]) {
    s.open_proposal(wi, "change", "test", reqs, "test")
        .await
        .unwrap();
    s.set_plan(wi, json!({"summary": "x", "changes": []}), "orchestrator")
        .await
        .unwrap();
    s.transition(wi, ProposalState::Approved, "human", "")
        .await
        .unwrap();
    s.transition(wi, ProposalState::Building, "orchestrator", "")
        .await
        .unwrap();
}

/// Record formal passes for every affected test at its overlay closure.
async fn pass_affected(s: &Store, wi: &str) {
    let (g, touched) = s.overlay_graph(wi).await.unwrap();
    for t in s.affected_tests(wi, &g, &touched).await.unwrap() {
        let sym = g.symbol(&t).unwrap();
        s.record_test_log(
            json!({"test": t, "test_version": sym.version, "closure_hash": g.closure_hash(&t).unwrap(),
                   "status": "pass", "formal": true, "output": "ok"}),
            None,
        )
        .await
        .unwrap();
    }
}

async fn to_ready(s: &Store, wi: &str) {
    s.transition(wi, ProposalState::Reviewing, "orchestrator", "")
        .await
        .unwrap();
    s.transition(wi, ProposalState::Testing, "orchestrator", "")
        .await
        .unwrap();
    s.transition(wi, ProposalState::Ready, "orchestrator", "")
        .await
        .unwrap();
}

async fn propose_body(s: &Store, wi: &str, symbol: &str, prefix: &str, by: &str) {
    let node = s.symbol_id(symbol).await.unwrap();
    let v = s.version_in(&node, None).await.unwrap().unwrap();
    let doc = s.get_node(&v).await.unwrap();
    s.propose_version(
        wi,
        &ProposeVersion {
            symbol: Some(symbol.into()),
            signature: doc["signature"].as_str().unwrap().into(),
            body: format!("{prefix}\n{}", doc["body"].as_str().unwrap()),
            by: Some(by.into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn proposals_are_accepted_all_or_nothing_into_the_project_version() {
    let s = temp_store("o4prop").await.unwrap();
    import_dir(&s, &seed_dir()).await.unwrap();
    let pv = s.current_version().await.unwrap();
    let vc = s.symbol_id("verify_credentials").await.unwrap();
    let before = s.tree(None).await.unwrap();

    // a work item names its open project version (has_work_item)
    let a = work_item(&s, "code/verify_credentials").await;
    let w = s.work_item(&a).await.unwrap();
    assert_eq!(w["project_version"], json!(pv));
    let edge = s
        .aql(
            "FOR e IN @@has_work_item FILTER e._to == CONCAT('work_items/', @k) RETURN e._from",
            json!({"k": a}),
        )
        .await
        .unwrap();
    assert_eq!(edge, vec![json!(pv)]);

    // a proposal: the candidate version exists but no project version sees it
    building(&s, &a, &[]).await;
    propose_body(&s, &a, "verify_credentials", "// A", "coder").await;
    assert_eq!(
        s.tree(None).await.unwrap().version(&vc),
        before.version(&vc)
    );
    let cand = s
        .aql(
            "FOR e IN @@has_version FILTER e._from == @n AND e.work_item == @w RETURN e.state",
            json!({"n": vc, "w": a}),
        )
        .await
        .unwrap();
    assert_eq!(cand, vec![json!("candidate")]);

    // gates: tests not run, no review → refused
    to_ready(&s, &a).await;
    let err = s.accept_proposal(&a, "test", false).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    // a review by the author never counts
    pass_affected(&s, &a).await;
    s.submit_review(&a, &[], "self-review", "coder", "engine_reviewer")
        .await
        .unwrap();
    let (c, _, _) = s.accept_checks(&a).await.unwrap();
    assert!(c.tests_ok, "{c:?}");
    assert!(!c.review_ok && !c.review_attested);
    s.submit_review(&a, &[], "lgtm", "reviewer", "engine_reviewer")
        .await
        .unwrap();
    let (c, _, _) = s.accept_checks(&a).await.unwrap();
    assert!(c.ok(), "{c:?}");

    // a second work item on a disjoint component, built from the same base
    let b = work_item(&s, "code/hash_password").await;
    building(&s, &b, &[]).await;
    propose_body(&s, &b, "hash_password", "// B", "coder").await;

    // accept A: the tree moves; the candidate is accepted
    s.accept_proposal(&a, "test", false).await.unwrap();
    let after_a = s.tree(None).await.unwrap();
    assert_ne!(after_a.version(&vc), before.version(&vc));
    assert_eq!(
        s.get_proposal(&a).await.unwrap()["state"],
        json!("accepted")
    );
    let st = s
        .aql(
            "FOR e IN @@has_version FILTER e._from == @n AND e.work_item == @w RETURN e.state",
            json!({"n": vc, "w": a}),
        )
        .await
        .unwrap();
    assert_eq!(st, vec![json!("accepted")]);

    // accept B afterwards: built against the tree as it is now, so A's change survives
    to_ready(&s, &b).await;
    pass_affected(&s, &b).await;
    s.submit_review(&b, &[], "lgtm", "reviewer", "engine_reviewer")
        .await
        .unwrap();
    s.accept_proposal(&b, "test", false).await.unwrap();
    let after_b = s.tree(None).await.unwrap();
    let hp = s.symbol_id("hash_password").await.unwrap();
    assert_eq!(after_b.version(&vc), after_a.version(&vc), "no lost update");
    assert_ne!(after_b.version(&hp), after_a.version(&hp));
    assert!(s.check_invariants().await.unwrap().is_empty());

    // the orphan gate: new code no requirement reaches is refused (and nothing changes); a
    // context with no requirements holds a component for it
    s.insert_node("contexts", json!({"_key": "billing", "title": "Billing"}))
        .await
        .unwrap();
    s.insert_edge(
        "contains",
        &s.project_node().unwrap(),
        "contexts/billing",
        json!({}),
    )
    .await
    .unwrap();
    s.insert_node(
        "components",
        json!({"_key": "invoices", "title": "Invoices"}),
    )
    .await
    .unwrap();
    s.insert_edge(
        "contains",
        "contexts/billing",
        "components/invoices",
        json!({}),
    )
    .await
    .unwrap();
    let after_b = s.tree(None).await.unwrap();
    let c = work_item(&s, "components/invoices").await;
    building(&s, &c, &[]).await;
    s.propose_version(
        &c,
        &ProposeVersion {
            display_name: Some("lonely".into()),
            kind: Some(Kind::Function),
            language: Some(Language::Rust),
            signature: "() -> i32".into(),
            body: "42".into(),
            component: Some("invoices".into()),
            by: Some("coder".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let dry = s.accept_proposal(&c, "test", true).await.unwrap();
    assert_eq!(dry.orphans_ok, Some(false), "{dry:?}");
    assert!(dry.new_orphans.iter().any(|o| o.ends_with(".lonely")));
    let rev = s.tree(None).await.unwrap().rev;
    assert_eq!(rev, after_b.rev, "a dry run changes nothing");

    // with blind review off, an unreviewed proposal passes the review gate
    s.reject_proposal(&c, "test", "orphan").await.unwrap();
    let p = s.project_id().unwrap();
    s.register_user("owner", "password123", "Owner")
        .await
        .unwrap();
    let (tok, _) = s.login("owner", "password123").await.unwrap();
    let owner = s.authenticate(&tok).await.unwrap();
    s.set_setting(
        &owner,
        &format!("project:{p}"),
        "blind_review",
        Some(json!(false)),
    )
    .await
    .unwrap();
    let d = work_item(&s, "code/verify_password").await;
    building(&s, &d, &[]).await;
    propose_body(&s, &d, "verify_password", "// D", "coder").await;
    to_ready(&s, &d).await;
    pass_affected(&s, &d).await;
    let (c, _, _) = s.accept_checks(&d).await.unwrap();
    assert!(!c.review_required && c.review_ok, "{c:?}");
    s.accept_proposal(&d, "test", false).await.unwrap();
    s.drop_db().await.unwrap();
}
