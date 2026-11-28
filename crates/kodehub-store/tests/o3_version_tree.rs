//! Ontology step 3 (D-057): the version tree.
use kodehub_store::seed::import_dir;
use kodehub_store::testutil::{seed_dir, temp_store};
use kodehub_store::tree::TreeChanges;
use kodehub_store::{version_content_from_doc, Store, StoreError};
use serde_json::json;

async fn count(s: &Store, coll: &str) -> usize {
    s.aql("RETURN COUNT(FOR x IN @@c RETURN 1)", json!({"@c": coll}))
        .await
        .unwrap()[0]
        .as_u64()
        .unwrap() as usize
}

async fn edit(s: &Store, key: &str, pv: Option<&str>, f: impl Fn(&str) -> String) {
    let node = s.symbol_id(key).await.unwrap();
    let v = s
        .tree(pv)
        .await
        .unwrap()
        .version(&node)
        .unwrap()
        .to_string();
    let doc = s.get_node(&v).await.unwrap();
    let mut c = version_content_from_doc(&doc).unwrap();
    c.body = f(&c.body);
    let (vid, _) = s
        .put_leaf_version(&node, &c, "test", None, None)
        .await
        .unwrap();
    let mut ch = TreeChanges::default();
    ch.leaves.insert(node, Some(vid));
    s.apply_tree_changes(pv, &ch, "test", None).await.unwrap();
}

#[tokio::test]
async fn the_version_tree() {
    let s = temp_store("o3tree").await.unwrap();
    let report = import_dir(&s, &seed_dir()).await.unwrap();
    assert_eq!(report.symbols_created, 22);
    assert!(s.check_invariants().await.unwrap().is_empty());
    let r1 = s.current_version().await.unwrap();
    let t1 = s.tree(None).await.unwrap();
    // every code and test node, every C4 node is in the first project version
    assert_eq!(t1.nodes_in("code").count(), 13);
    assert_eq!(t1.nodes_in("tests").count(), 9);
    assert_eq!(t1.nodes_in("components").count(), 2);
    assert_eq!(t1.nodes_in("containers").count(), 1);
    assert_eq!(t1.nodes_in("contexts").count(), 1);
    let vc = s.symbol_id("verify_credentials").await.unwrap();
    let comp = s.db.qid("components/credential-verification").unwrap();
    assert_eq!(t1.ancestors(&vc)[1], comp);
    // a re-import of unchanged data is a no-op
    let before: Vec<usize> = futures(&s).await;
    let again = import_dir(&s, &seed_dir()).await.unwrap();
    assert_eq!(
        again.versions_created + again.symbols_changed + again.symbols_created,
        0
    );
    assert_eq!(
        again.edges_created + again.documents_created + again.documents_updated,
        0
    );
    assert_eq!(futures(&s).await, before);
    assert_eq!(s.tree(None).await.unwrap().rev, t1.rev);

    // changing one code node: one new code version, one new version per ancestor level
    let counts = futures(&s).await;
    edit(&s, "verify_credentials", None, |b| format!("// tweak\n{b}")).await;
    let after = futures(&s).await;
    // order: code_versions, component_versions, container_versions, context_versions
    assert_eq!(
        after
            .iter()
            .zip(&counts)
            .map(|(a, b)| a - b)
            .collect::<Vec<_>>(),
        vec![1, 1, 1, 1],
        "exactly one new version per level"
    );
    let t2 = s.tree(None).await.unwrap();
    assert_eq!(t2.rev, t1.rev + 1);
    assert_ne!(t2.version(&vc), t1.version(&vc));
    assert_ne!(t2.version(&comp), t1.version(&comp));
    // the untouched component and every untouched node keep their version ids
    let ph = s.db.qid("components/password-hashing").unwrap();
    assert_eq!(t2.version(&ph), t1.version(&ph));
    let hp = s.symbol_id("hash_password").await.unwrap();
    assert_eq!(t2.version(&hp), t1.version(&hp));
    // the new component version lists all its children, changed or not
    let cv = s.get_node(t2.version(&comp).unwrap()).await.unwrap();
    assert_eq!(
        cv["children"].as_array().unwrap().len(),
        t1.child_nodes(&comp).len()
    );
    assert!(s.check_invariants().await.unwrap().is_empty());

    // a second project version starts from the first and shares every version
    s.freeze_project_version(&r1, "test", "milestone")
        .await
        .unwrap();
    let err = s
        .apply_tree_changes(
            Some(&r1),
            &TreeChanges {
                leaves: [(vc.clone(), None)].into(),
                ..Default::default()
            },
            "test",
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Immutable(_)),
        "a frozen tree never changes: {err:?}"
    );
    // freezing the current version opens the next one, based on it, as current
    let r2 = s.current_version().await.unwrap();
    assert_ne!(r2, r1);
    let r2doc = s.get_node(&r2).await.unwrap();
    assert_eq!(r2doc["state"], "open");
    assert_eq!(r2doc["based_on"], json!(r1));
    let t2b = s.tree(Some(&r2)).await.unwrap();
    assert_eq!(
        t2b.version_of, t2.version_of,
        "a new project version starts with the same tree"
    );
    edit(&s, "hash_password", Some(&r2), |b| format!("// r2\n{b}")).await;
    let t3 = s.tree(Some(&r2)).await.unwrap();
    let r1_tree = s.tree(Some(&r1)).await.unwrap();
    assert_eq!(
        r1_tree.version_of, t2.version_of,
        "the frozen version is unchanged"
    );
    assert_ne!(t3.version(&hp), r1_tree.version(&hp));
    // unchanged branches are the same documents in both
    assert_eq!(t3.version(&comp), r1_tree.version(&comp));
    assert_eq!(t3.version(&vc), r1_tree.version(&vc));
    let diff: Vec<&String> = t3
        .version_of
        .iter()
        .filter(|(n, v)| r1_tree.version(n) != Some(v.as_str()))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        diff.len(),
        4,
        "hash_password and its three ancestors: {diff:?}"
    );

    // compare-and-set: a change built on an old revision is refused
    let stale = t3.rev - 1;
    let err = s
        .apply_tree_changes(
            Some(&r2),
            &TreeChanges {
                leaves: [(hp.clone(), r1_tree.version(&hp).map(str::to_string))].into(),
                ..Default::default()
            },
            "test",
            Some(stale),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");

    // history names the project versions holding each version
    let hist = s.version_history("hash_password").await.unwrap();
    assert_eq!(hist.len(), 2);
    assert!(hist.iter().any(|h| h["project_versions"] == json!(["r1"])));
    assert!(hist.iter().any(|h| h["project_versions"] == json!(["r2"])));

    // requirements still attach to unversioned nodes and survive every change
    let req = s.requirements_for(&vc).await.unwrap();
    let keys: Vec<&str> = req["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| kodehub_store::arango::local_key(r["key"].as_str().unwrap()))
        .collect();
    assert!(keys.contains(&"r1-generic-failure"));
    assert!(
        keys.contains(&"r2-hashed-storage"),
        "inherited from the context"
    );
    assert_eq!(
        req["chain"].as_array().unwrap().last().unwrap(),
        &json!(s.project_node().unwrap())
    );
    s.drop_db().await.unwrap();
}

/// Version document counts: code, component, container, context.
async fn futures(s: &Store) -> Vec<usize> {
    let mut v = Vec::new();
    for c in [
        "code_versions",
        "component_versions",
        "container_versions",
        "context_versions",
    ] {
        v.push(count(s, c).await);
    }
    v
}
