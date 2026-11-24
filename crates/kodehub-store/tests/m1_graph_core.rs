//! M1 acceptance: graph core against a real ArangoDB (one throwaway database per test).

use std::collections::BTreeSet;

use kodehub_store::seed::import_dir;
use kodehub_store::testutil::{seed_dir, temp_store};
use kodehub_store::{version_content_from_doc, Store, StoreError};
use serde_json::json;

async fn seeded(prefix: &str) -> Store {
    let s = temp_store(prefix)
        .await
        .expect("arangodb reachable (run `make db`)");
    import_dir(&s, &seed_dir()).await.expect("seed imports");
    s
}

async fn count(s: &Store, coll: &str) -> usize {
    s.aql("RETURN LENGTH(@@c)", json!({"@c": coll}))
        .await
        .unwrap()[0]
        .as_u64()
        .unwrap() as usize
}

#[tokio::test]
async fn seed_imports_cleanly_and_reimport_is_a_noop() {
    let s = seeded("reimport").await;
    assert!(s.check_invariants().await.unwrap().is_empty());
    let versions = count(&s, "versions").await;
    let symbols = count(&s, "symbols").await;
    // check_password duplicates verify_password, so they share one version node
    assert_eq!(versions, symbols - 1);
    let report = import_dir(&s, &seed_dir()).await.unwrap();
    assert_eq!(report.versions_created, 0);
    assert_eq!(report.symbols_changed, 0);
    assert_eq!(report.symbols_created, 0);
    assert_eq!(report.edges_created, 0);
    assert_eq!(report.documents_created, 0);
    assert_eq!(report.documents_updated, 0);
    assert_eq!(count(&s, "versions").await, versions);
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn changing_one_body_creates_one_version_and_flips_edges() {
    let s = seeded("flip").await;
    let before_versions = count(&s, "versions").await;
    let old = s
        .aql(
            "FOR e IN @@current_version FILTER e._to == CONCAT(@pfx, 'symbols/verify_credentials') RETURN e._from",
            json!({}),
        )
        .await
        .unwrap()[0]
        .as_str()
        .unwrap()
        .to_string();
    let doc = s.get_node(&old).await.unwrap();
    let mut content = version_content_from_doc(&doc).unwrap();
    content.body = format!("// tweak\n{}", content.body);
    let out = s
        .write_version("verify_credentials", &content, "test")
        .await
        .unwrap();
    assert!(out.created_version && out.changed);
    assert_eq!(count(&s, "versions").await, before_versions + 1);
    let cur = s
        .aql(
            "FOR e IN @@current_version FILTER e._to == CONCAT(@pfx, 'symbols/verify_credentials') RETURN e._from",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(cur, vec![json!(format!("versions/{}", out.version))]);
    let prev = s
        .aql(
            "FOR e IN @@previous_version FILTER e._to == CONCAT(@pfx, 'symbols/verify_credentials') RETURN e",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(prev.len(), 1);
    assert_eq!(prev[0]["_from"], json!(old));
    assert_eq!(prev[0]["superseded_by"], json!(out.version));
    assert!(prev[0]["superseded_at"].is_string());
    // writing the same content again is a no-op
    let again = s
        .write_version("verify_credentials", &content, "test")
        .await
        .unwrap();
    assert!(!again.changed && !again.created_version);
    // version nodes are immutable through the API
    let err = s
        .update_node("versions", &out.version, &json!({"body": "x"}))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Immutable(_)));
    assert!(s.check_invariants().await.unwrap().is_empty());
    let hist = s.version_history("verify_credentials").await.unwrap();
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0]["current"], json!(true));
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn changing_a_dependency_changes_exactly_the_dependents_closure_hashes() {
    let s = seeded("closure").await;
    let snapshot = |s: Store| async move {
        s.aql(
            "FOR x IN @@symbols SORT x._key RETURN [x._key, x.closure_hash]",
            json!({}),
        )
        .await
        .unwrap()
    };
    let before = snapshot(s.clone()).await;
    let graph = s.load_code_graph().await.unwrap();
    let mut expected: BTreeSet<String> = graph.dependents("hash_password");
    expected.insert("hash_password".into());
    assert!(expected.contains("UserStore-add_user"));
    assert!(expected.contains("test_unknown_user_is_invalid"));
    assert!(!expected.contains("verify_credentials"));

    let doc = s.aql("FOR e IN @@current_version FILTER e._to == CONCAT(@pfx, 'symbols/hash_password') RETURN DOCUMENT(e._from)", json!({})).await.unwrap();
    let mut content = version_content_from_doc(&doc[0]).unwrap();
    content.body.push_str("\n// changed");
    s.write_version("hash_password", &content, "test")
        .await
        .unwrap();
    let after = snapshot(s.clone()).await;
    let changed: BTreeSet<String> = before
        .iter()
        .zip(after.iter())
        .filter(|(a, b)| a[1] != b[1])
        .map(|(a, _)| a[0].as_str().unwrap().to_string())
        .collect();
    assert_eq!(changed, expected);
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_second_current_version_edge_is_rejected() {
    let s = seeded("invariant").await;
    let v = s.aql("FOR e IN @@current_version FILTER e._to == CONCAT(@pfx, 'symbols/hash_password') RETURN PARSE_IDENTIFIER(e._from).key", json!({})).await.unwrap();
    // even pointing the *same* version at it again is refused
    let err = s
        .force_add_current_version(v[0].as_str().unwrap(), "hash_password")
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(_)), "{err:?}");
    // and the generic edge API refuses protected collections outright
    let err = s
        .insert_edge(
            "current_version",
            "versions/x",
            "symbols/hash_password",
            json!({}),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)));
    assert!(s.check_invariants().await.unwrap().is_empty());
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn requirement_edits_keep_history() {
    let s = seeded("reqhist").await;
    let (new, changed) = s.update_requirement(
        "r1-generic-failure",
        &json!({"edge_cases": [{"description": "Locked account", "input": {}, "expected": "invalid_credentials"}]}),
        "human", true).await.unwrap();
    assert!(changed);
    assert_eq!(new["revision"], json!(2));
    assert_eq!(new["at_risk"], json!(true));
    let hist = s.requirement_history("r1-generic-failure").await.unwrap();
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0]["revision"], json!(1));
    assert_eq!(
        hist[0]["snapshot"]["edge_cases"][0]["description"],
        json!("Empty username")
    );
    s.drop_db().await.unwrap();
}
