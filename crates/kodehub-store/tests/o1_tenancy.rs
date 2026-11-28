//! Ontology step 1 (D-055): every project shares one set of collections.

use kodehub_store::seed::import_dir;
use kodehub_store::testutil::{seed_dir, temp_store};
use kodehub_store::StoreError;
use reqwest::Method;
use serde_json::json;

async fn collection_count(s: &kodehub_store::Store) -> usize {
    s.db.db_raw(Method::GET, "/_api/collection?excludeSystem=true", None)
        .await
        .unwrap()["result"]
        .as_array()
        .unwrap()
        .len()
}

#[tokio::test]
async fn projects_with_identical_content_share_collections_but_never_data() {
    let s = temp_store("tenancy").await.unwrap();
    let before = collection_count(&s).await;
    let a = s.create_project("Alpha", "").await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = s.create_project("Beta", "").await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // adding projects never adds collections
    assert_eq!(collection_count(&s).await, before);

    let (sa, sb) = (s.for_project(&a), s.for_project(&b));
    import_dir(&sa, &seed_dir()).await.unwrap();
    import_dir(&sb, &seed_dir()).await.unwrap();

    // same code in both projects: same content hashes, different (qualified) keys
    let ca = sa
        .aql(
            "FOR v IN @@code_versions SORT v.content_hash RETURN v",
            json!({}),
        )
        .await
        .unwrap();
    let cb = sb
        .aql(
            "FOR v IN @@code_versions SORT v.content_hash RETURN v",
            json!({}),
        )
        .await
        .unwrap();
    assert!(!ca.is_empty());
    assert_eq!(ca.len(), cb.len(), "a scan sees only its own project");
    for (x, y) in ca.iter().zip(&cb) {
        assert_eq!(x["content_hash"], y["content_hash"]);
        assert_ne!(x["_key"], y["_key"]);
        assert_eq!(x["project"], json!(a));
        assert_eq!(y["project"], json!(b));
        assert!(x["_key"].as_str().unwrap().starts_with(&format!("{a}.")));
    }
    // the raw collection holds both projects
    let total =
        s.db.aql_raw("RETURN COUNT(FOR v IN code_versions RETURN 1)", json!({}))
            .await
            .unwrap()[0]
            .as_u64()
            .unwrap() as usize;
    assert_eq!(total, ca.len() + cb.len());

    // reading another project's document by key is refused
    let foreign = cb[0]["_key"].as_str().unwrap();
    let err = sa.db.get("code_versions", foreign, None).await.unwrap_err();
    assert!(err.is_not_found(), "{err:?}");
    // a local key resolves inside the reading project
    let local = kodehub_store::arango::local_key(ca[0]["_key"].as_str().unwrap());
    assert_eq!(
        sa.db.get("code_versions", local, None).await.unwrap()["_key"],
        ca[0]["_key"]
    );

    // writing a document keyed for another project is refused
    let err = sa
        .db
        .insert(
            "requirements",
            &json!({"_key": format!("{b}.x"), "statement": "x"}),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(_)), "{err:?}");
    // so is an edge to another project's node
    let mine = format!("code/{}", sa.db.pkey("hash_password").unwrap());
    let theirs = format!("code/{}", sb.db.pkey("hash_password").unwrap());
    let err = sa
        .db
        .insert(
            "calls",
            &json!({"_key": sa.db.pkey("x").unwrap(), "_from": mine, "_to": theirs}),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(_)), "{err:?}");
    // and an unqualified id in a query is caught instead of silently matching nothing
    let err = sa
        .aql(
            "FOR e IN @@calls FILTER e._from == @n RETURN e",
            json!({"n": "code/hash_password"}),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(_)), "{err:?}");

    // deleting a project removes its documents and leaves the other project intact
    s.switch_project(&a).await.unwrap();
    s.delete_project(&b).await.unwrap();
    let left =
        s.db.aql_raw(
            "RETURN COUNT(FOR v IN code_versions FILTER v.project == @p RETURN 1)",
            json!({"p": b}),
        )
        .await
        .unwrap()[0]
            .as_u64()
            .unwrap();
    assert_eq!(left, 0);
    assert_eq!(
        sa.aql("RETURN COUNT(FOR v IN @@code_versions RETURN 1)", json!({}))
            .await
            .unwrap()[0]
            .as_u64()
            .unwrap() as usize,
        ca.len()
    );
    assert_eq!(collection_count(&s).await, before);
    s.drop_db().await.unwrap();
}
