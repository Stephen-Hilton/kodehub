//! M1 acceptance (graph core) on the ontology model: closure hashes per project version,
//! requirement versions, invariants. Seeding, re-import and one-change-one-version-per-level
//! are covered by `o3_version_tree`.

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

#[tokio::test]
async fn changing_a_dependency_changes_exactly_the_dependents_closure_hashes() {
    let s = seeded("closure").await;
    let k = |x: &str| s.db.pkey(x).unwrap();
    let before = s.load_code_graph().await.unwrap();
    let mut expected: BTreeSet<String> = before.dependents(&k("hash_password"));
    expected.insert(k("hash_password"));
    assert!(expected.contains(&k("UserStore-add_user")));
    assert!(expected.contains(&k("test_unknown_user_is_invalid")));
    assert!(!expected.contains(&k("verify_credentials")));

    let node = s.symbol_id("hash_password").await.unwrap();
    let v = s.version_in(&node, None).await.unwrap().unwrap();
    let mut content = version_content_from_doc(&s.get_node(&v).await.unwrap()).unwrap();
    content.body.push_str("\n// changed");
    let out = s
        .write_version("hash_password", &content, "test")
        .await
        .unwrap();
    assert!(out.changed && out.created_version);
    let after = s.load_code_graph().await.unwrap();
    let changed: BTreeSet<String> = before
        .symbols
        .keys()
        .filter(|id| before.closure_hash(id).ok() != after.closure_hash(id).ok())
        .cloned()
        .collect();
    assert_eq!(changed, expected);
    // writing the same content again is a no-op
    let again = s
        .write_version("hash_password", &content, "test")
        .await
        .unwrap();
    assert!(!again.changed && !again.created_version);
    // version nodes are immutable through the API
    let err = s
        .update_node("code_versions", &out.version, &json!({"body": "x"}))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Immutable(_)));
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_node_appears_once_per_project_version() {
    let s = seeded("invariant").await;
    assert!(s.check_invariants().await.unwrap().is_empty());
    // the generic edge API refuses the protected collections outright
    let node = s.symbol_id("hash_password").await.unwrap();
    let err = s
        .insert_edge("has_version", &node, "code_versions/x", json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)));
    // a second version of a node wired into the tree by hand is reported
    let tree = s.tree(None).await.unwrap();
    let cv = tree.version(&node).unwrap().to_string();
    let comp = tree
        .version(&s.db.qid("components/password-hashing").unwrap())
        .unwrap()
        .to_string();
    let doc = s.get_node(&cv).await.unwrap();
    let mut c = version_content_from_doc(&doc).unwrap();
    c.body.push_str("\n// sneaky");
    let (vid, _) = s
        .put_leaf_version(&node, &c, "test", None, None)
        .await
        .unwrap();
    s.db.insert(
        "contains",
        &json!({"_key": s.db.pkey("sneak").unwrap(), "_from": comp, "_to": vid}),
        None,
    )
    .await
    .unwrap();
    let problems = s.check_invariants().await.unwrap();
    assert!(
        problems.iter().any(|p| p.contains("appears 2 times")),
        "{problems:?}"
    );
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn requirement_edits_are_versions() {
    let s = seeded("reqhist").await;
    let (new, changed) = s.update_requirement(
        "r1-generic-failure",
        &json!({"edge_cases": [{"description": "Locked account", "input": {}, "expected": "invalid_credentials"}]}),
        "human", true).await.unwrap();
    assert!(changed);
    assert_eq!(new["revision"], json!(2));
    assert_eq!(new["at_risk"], json!(true));
    let hist = s.requirement_history("r1-generic-failure").await.unwrap();
    assert_eq!(hist.len(), 2, "every revision is a version");
    assert_eq!(hist[0]["revision"], json!(2));
    assert_eq!(hist[0]["state"], json!("accepted"));
    assert_eq!(
        hist[1]["content"]["edge_cases"][0]["description"],
        json!("Empty username")
    );
    // versions hang off the requirement node
    let rid = new["_id"].as_str().unwrap();
    let n = s
        .aql(
            "RETURN COUNT(FOR e IN @@has_version FILTER e._from == @r RETURN 1)",
            json!({"r": rid}),
        )
        .await
        .unwrap();
    assert_eq!(n[0], 2);
    // decisions are requirements of kind `decision`
    let ds = s.list_decisions().await.unwrap();
    assert_eq!(ds.len(), 1);
    assert_eq!(
        ds[0]["justifies"],
        json!([s.symbol_id("verify_credentials").await.unwrap()])
    );
    s.drop_db().await.unwrap();
}
