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
            "FOR e IN @@has_version FILTER e._from == CONCAT(@pfx, 'symbols/verify_credentials') AND e.state == 'stable' RETURN e._to",
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
            "FOR e IN @@has_version FILTER e._from == CONCAT(@pfx, 'symbols/verify_credentials') AND e.state == 'stable' RETURN e._to",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(cur, vec![json!(format!("versions/{}", out.version))]);
    // the old version is retired, not deleted: its edge closed with validTo (type 2 SCD)
    let prev = s
        .aql(
            "FOR e IN @@has_version FILTER e._from == CONCAT(@pfx, 'symbols/verify_credentials') AND e.state == 'retired' RETURN e",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(prev.len(), 1);
    assert_eq!(prev[0]["_to"], json!(old));
    assert_eq!(prev[0]["retired_by"], json!(out.version));
    assert!(prev[0]["validTo"].is_string());
    let new_edge = s
        .stable_edge_in("symbols/verify_credentials", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        new_edge["validFrom"], prev[0]["validTo"],
        "intervals meet, never overlap"
    );
    assert_eq!(
        new_edge["seq"].as_i64(),
        Some(prev[0]["seq"].as_i64().unwrap() + 1)
    );
    // as-of lookup returns the version that was stable at that moment
    let before = prev[0]["validFrom"].as_str().unwrap().to_string();
    let asof = s
        .version_as_of("symbols/verify_credentials", &before)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        format!("versions/{}", asof["version"].as_str().unwrap()),
        old
    );
    let now = kodehub_core::model::now_iso();
    let asof = s
        .version_as_of("symbols/verify_credentials", &now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(asof["version"], json!(out.version));
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

    let doc = s.aql("FOR e IN @@has_version FILTER e._from == CONCAT(@pfx, 'symbols/hash_password') AND e.state == 'stable' RETURN DOCUMENT(e._to)", json!({})).await.unwrap();
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
async fn a_second_stable_version_edge_is_rejected() {
    let s = seeded("invariant").await;
    let v = s.aql("FOR e IN @@has_version FILTER e._from == CONCAT(@pfx, 'symbols/hash_password') AND e.state == 'stable' RETURN PARSE_IDENTIFIER(e._to).key", json!({})).await.unwrap();
    // even pointing the *same* version at it again is refused
    let err = s
        .force_add_stable_version(v[0].as_str().unwrap(), "hash_password")
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(_)), "{err:?}");
    // and the generic edge API refuses protected collections outright
    let err = s
        .insert_edge(
            "has_version",
            "symbols/hash_password",
            "versions/x",
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
