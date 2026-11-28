//! Ontology step 5 (D-059, D-060): project versions are the releases; deployments carry one
//! into one environment through that environment's deployers.
use kodehub_store::seed::import_dir;
use kodehub_store::testutil::{seed_dir, temp_store};
use kodehub_store::{version_content_from_doc, Store, StoreError};
use serde_json::json;

async fn deploy(s: &Store, pv: &str, env: &str, ordered: bool) -> serde_json::Value {
    let d = s
        .create_deployment(pv, env, "test", "promote", ordered)
        .await
        .unwrap();
    let k = d["_key"].as_str().unwrap().to_string();
    if d["state"] == "awaiting_approval" {
        return d;
    }
    s.start_deployment(&k).await.unwrap();
    s.finish_deployment(&k, true, json!({"ok": true}), "test")
        .await
        .unwrap()
}

#[tokio::test]
async fn deployments_promote_freeze_supersede_and_retry() {
    let s = temp_store("o5deploy").await.unwrap();
    import_dir(&s, &seed_dir()).await.unwrap();
    let envs = s.environments().await.unwrap();
    let names: Vec<&str> = envs.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["dev", "test", "qa", "preprod", "prod"]);
    // one deployer per (environment, deploy target), each running the deploy image
    for e in &envs {
        let ds = e["deployers"].as_array().unwrap();
        assert_eq!(ds.len(), 1, "{e}");
        assert_eq!(ds[0]["kind"], "k8s_namespace");
        assert_eq!(ds[0]["target"], "default");
    }
    let runs = s
        .aql("RETURN COUNT(FOR e IN @@runs RETURN 1)", json!({}))
        .await
        .unwrap();
    assert_eq!(runs[0], 5);

    let r1 = s.current_version().await.unwrap();
    // DEV: the version stays open
    let d = deploy(&s, &r1, "dev", true).await;
    assert_eq!(d["state"], "succeeded");
    assert_eq!(s.get_node(&r1).await.unwrap()["state"], "open");
    assert_eq!(s.running_in(&r1).await.unwrap(), vec!["dev"]);
    // a deployment's environment and project version are both linked; its deployers too
    assert_eq!(d["deployers"].as_array().unwrap().len(), 1);
    // promotion is ordered: not straight to QA
    let err = s
        .create_deployment(&r1, "qa", "test", "promote", true)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
    // TEST: the version is cut, and the next one opens as current
    deploy(&s, &r1, "test", true).await;
    assert_eq!(s.get_node(&r1).await.unwrap()["state"], "frozen");
    let r2 = s.current_version().await.unwrap();
    assert_ne!(r2, r1);
    // a frozen version refuses changes; work goes on in r2
    let vc = s.symbol_id("verify_credentials").await.unwrap();
    let v = s.version_in(&vc, Some(&r1)).await.unwrap().unwrap();
    let mut c = version_content_from_doc(&s.get_node(&v).await.unwrap()).unwrap();
    c.body = format!("// r2\n{}", c.body);
    s.write_version("verify_credentials", &c, "test")
        .await
        .unwrap();
    assert_ne!(s.version_in(&vc, Some(&r2)).await.unwrap(), Some(v.clone()));
    assert_eq!(s.version_in(&vc, Some(&r1)).await.unwrap(), Some(v.clone()));

    // QA: a failed attempt is retried; every attempt is logged
    let q = s
        .create_deployment(&r1, "qa", "test", "promote", true)
        .await
        .unwrap();
    let qk = q["_key"].as_str().unwrap().to_string();
    s.start_deployment(&qk).await.unwrap();
    let f = s
        .finish_deployment(
            &qk,
            false,
            json!({"ok": false, "why": "cloud hiccup"}),
            "test",
        )
        .await
        .unwrap();
    assert_eq!(f["state"], "failed");
    s.start_deployment(&qk).await.unwrap();
    let ok = s
        .finish_deployment(&qk, true, json!({"ok": true}), "test")
        .await
        .unwrap();
    assert_eq!(ok["state"], "succeeded");
    assert_eq!(ok["attempts"], 2);
    let steps: Vec<&str> = ok["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        steps,
        vec!["ok", "running", "failed", "running", "succeeded"],
        "{ok}"
    );
    deploy(&s, &r1, "preprod", true).await;

    // PROD needs a human approval before it can start
    let p = deploy(&s, &r1, "prod", true).await;
    assert_eq!(p["state"], "awaiting_approval");
    let pk = p["_key"].as_str().unwrap().to_string();
    assert!(s.start_deployment(&pk).await.is_err());
    s.approve_deployment(&pk, "users/boss").await.unwrap();
    s.start_deployment(&pk).await.unwrap();
    let p = s
        .finish_deployment(&pk, true, json!({"ok": true}), "test")
        .await
        .unwrap();
    assert_eq!(p["approved_by"], "users/boss");
    assert_eq!(s.running_in(&r1).await.unwrap().len(), 5);

    // a hotfix of the frozen PROD version, while r2 stays open as the next release
    let hf = s.create_hotfix(&r1, None, "test").await.unwrap();
    let hf = hf["_id"].as_str().unwrap().to_string();
    assert_eq!(
        s.current_version().await.unwrap(),
        r2,
        "the hotfix is not current"
    );
    let hp = s.symbol_id("hash_password").await.unwrap();
    let hv = s.version_in(&hp, Some(&hf)).await.unwrap().unwrap();
    let mut hc = version_content_from_doc(&s.get_node(&hv).await.unwrap()).unwrap();
    hc.body = format!("// fix\n{}", hc.body);
    let (vid, _) = s
        .put_leaf_version(&hp, &hc, "test", None, None)
        .await
        .unwrap();
    s.apply_tree_changes(
        Some(&hf),
        &kodehub_store::tree::TreeChanges {
            leaves: [(hp.clone(), Some(vid))].into(),
            ..Default::default()
        },
        "test",
        None,
    )
    .await
    .unwrap();
    let diff = s.diff_project_versions(&r1, &hf).await.unwrap();
    assert_eq!(diff["changed"].as_array().unwrap().len(), 4, "{diff}");
    // hotfixes go straight to the environment they fix (not ordered) and supersede the version
    // serving the same targets there
    let h = s
        .create_deployment(&hf, "prod", "test", "hotfix", false)
        .await
        .unwrap();
    let hk = h["_key"].as_str().unwrap().to_string();
    s.approve_deployment(&hk, "users/boss").await.unwrap();
    s.start_deployment(&hk).await.unwrap();
    s.finish_deployment(&hk, true, json!({"ok": true}), "test")
        .await
        .unwrap();
    let prod: Vec<serde_json::Value> = s.current_deployments("prod").await.unwrap();
    assert_eq!(prod.len(), 1);
    assert_eq!(prod[0]["project_version"], json!(hf));
    assert_eq!(s.rollback_target("prod").await.unwrap(), Some(r1.clone()));
    // r1 still runs in the lower environments, unchanged
    assert!(s.running_in(&r1).await.unwrap().contains(&"qa".to_string()));
    s.drop_db().await.unwrap();
}
