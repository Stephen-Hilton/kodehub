//! S7 acceptance: build and test containers (local runtime: the same `kodehub-worker` entrypoint
//! as a subprocess), shell tests, build once / test many, formal runs, new failures become
//! high-priority rework, and failures already present at the baseline are not blamed.
mod common;

use common::*;
use kodehub_runner::jobs::JobRuntime;
use kodehub_server::orchestrator::Orchestrator;
use kodehub_store::queue::NewWorkItem;
use serde_json::json;

fn orchestrator(t: &TestServer, objects: &std::path::Path) -> Orchestrator {
    let r = t.state.runner.clone();
    let storage = format!("file://{}", objects.display());
    Orchestrator {
        store: t.store.clone(),
        runner: r.clone(),
        storage: kodehub_storage::StorageClient::new(&storage, None),
        jobs: JobRuntime::Local {
            worker: repo().join("target/debug/kodehub-worker"),
            env: vec![
                ("KODEHUB_HOME".into(), r.home.display().to_string()),
                (
                    "KODEHUB_CARGO_TARGET".into(),
                    r.cargo_target.display().to_string(),
                ),
                ("KODEHUB_PYENV".into(), r.pyenv.display().to_string()),
                ("KODEHUB_STORAGE_URL".into(), storage),
            ],
        },
        owner: "orch-test".into(),
        events: t.state.events.clone(),
    }
}

async fn run_item(t: &TestServer, o: &Orchestrator, target: &str) -> serde_json::Value {
    let p = t.store.project_id().unwrap();
    let w = t
        .store
        .enqueue(&NewWorkItem {
            project: p,
            agent_type: "test_run".into(),
            kind: "test_now".into(),
            target: Some(target.into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        o.tick().await.unwrap(),
        "the orchestrator claimed the test run"
    );
    let done = t
        .store
        .work_item(w["_key"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(done["state"], "done", "{done}");
    done["result"].clone()
}

#[tokio::test]
async fn containers_build_once_test_many_and_rework_new_failures() {
    let t = TestServer::start("s7c").await;
    let objects = tempfile::tempdir().unwrap();
    let o = orchestrator(&t, objects.path());
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;

    // whole project: one bundle per language is built, every test runs from it
    let r1 = run_item(&t, &o, "project").await;
    assert_eq!(r1["tests"], 9, "{r1}");
    assert_eq!(r1["failed"], 0, "{r1}");
    assert_eq!(r1["bundles_built"], 2);
    assert_eq!(r1["runtime"], "local");
    // the bundles are in storage, keyed by closure; artifact nodes link to the test versions
    let arts = t
        .store
        .aql(
            "FOR a IN @@artifacts FILTER a.storage_key != null RETURN a",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(arts.len(), 2);
    let links = t
        .store
        .aql("RETURN LENGTH(@@built_from)", json!({}))
        .await
        .unwrap();
    assert_eq!(links[0], 9);
    // formal runs are recorded
    let formal = t
        .store
        .aql(
            "FOR r IN @@test_runs FILTER r.formal == true RETURN r",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(formal.len(), 9);

    // built once, tested many times: the second run reuses both bundles
    let r2 = run_item(&t, &o, "project").await;
    assert_eq!(r2["bundles_built"], 0);
    assert_eq!(r2["bundles_reused"], 2);
    // a component runs only the tests that reach code beneath it (here the Rust tests, which
    // all go through verify_credentials → verify_password), from the already-built bundle
    let rc = run_item(&t, &o, "components/password-hashing").await;
    assert_eq!(rc["tests"], 5, "{rc}");
    assert_eq!(rc["bundles_built"], 0);
    assert_eq!(rc["bundles"].as_array().unwrap().len(), 1);

    // a broken helper: new failures become high-priority rework items, once
    let d = t.ok("GET", "/api/symbols/verify_password", None).await;
    let content = kodehub_core::model::VersionContent {
        kind: kodehub_core::model::Kind::Function,
        language: kodehub_core::model::Language::Rust,
        signature: d["version"]["signature"].as_str().unwrap().into(),
        body: "let _ = (password, hash);\nfalse".into(),
        attributes: json!({}),
    };
    t.store
        .write_version("verify_password", &content, "test")
        .await
        .unwrap();
    let r3 = run_item(&t, &o, "project").await;
    assert!(r3["failed"].as_u64().unwrap() > 0);
    assert_eq!(r3["bundles_built"], 1, "only the Rust bundle changed");
    let regressions = r3["regressions"].as_array().unwrap().clone();
    assert!(!regressions.is_empty());
    let p = t.store.project_id().unwrap();
    let rework: Vec<_> = t
        .store
        .list_work_items(Some(&p), Some("queued"), 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|w| w["kind"] == "fix_failure")
        .collect();
    assert_eq!(rework.len(), regressions.len());
    assert!(rework
        .iter()
        .all(|w| w["priority"] == 100 && w["agent_type"] == "coding"));
    let r4 = run_item(&t, &o, "project").await;
    assert!(
        r4["regressions"].as_array().unwrap().is_empty(),
        "already failing: not new"
    );

    // baseline: a changeset that does not fix the helper is not blamed for its failures
    let cs = t
        .ok(
            "POST",
            "/api/changesets",
            Some(json!({"title": "tweak", "intent": "x"})),
        )
        .await;
    let k = cs["_key"].as_str().unwrap().to_string();
    let vc = t.ok("GET", "/api/symbols/verify_credentials", None).await;
    t.ok("POST", &format!("/api/changesets/{k}/versions"), Some(json!({
        "symbol": "verify_credentials",
        "signature": vc["version"]["signature"], "body": format!("// tweak\n{}", vc["version"]["body"].as_str().unwrap()),
    }))).await;
    t.ok("POST", &format!("/api/changesets/{k}/test"), None)
        .await;
    let checks = t
        .ok("GET", &format!("/api/changesets/{k}/checks"), None)
        .await["checks"]
        .clone();
    let pre = checks["preexisting_failures"].as_array().unwrap();
    assert!(!pre.is_empty(), "{checks}");
    assert!(
        checks["failing_tests"].as_array().unwrap().is_empty(),
        "{checks}"
    );
    t.stop().await;
}
