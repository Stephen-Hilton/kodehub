//! S7 acceptance (runner): test bundles are built once and run as plain shell scripts.
mod common;

use common::*;
use kodehub_core::model::{Kind, Language};

fn tests_of(g: &kodehub_core::CodeGraph, lang: Language) -> Vec<String> {
    g.symbols
        .values()
        .filter(|s| s.kind == Kind::Test && s.language == lang)
        .map(|s| s.id.clone())
        .collect()
}

#[tokio::test]
async fn rust_and_python_bundles_build_once_run_as_shell() {
    let s = seeded("bundle").await;
    let r = runner();
    let g = s.load_code_graph().await.unwrap();

    // Rust: one compiled test binary, one script per test, exit status is the result
    let rt = tests_of(&g, Language::Rust);
    let (tar, m) = r
        .build_test_bundle(&g, Language::Rust, &rt)
        .await
        .unwrap()
        .expect("builds");
    assert_eq!(m.tests.len(), rt.len());
    assert_eq!(
        m.key,
        r.bundle_key(&g, Language::Rust, &rt).unwrap(),
        "keyed by closures"
    );
    let dir = tempfile::tempdir().unwrap();
    kodehub_runner::bundle::untar(&tar, dir.path()).unwrap();
    let script = std::fs::read_to_string(dir.path().join(&m.tests[0].script)).unwrap();
    assert!(script.starts_with("#!/bin/sh"), "{script}");
    assert!(
        script.contains("./bin/kodehub-tests --exact"),
        "runs the built binary, never cargo"
    );
    assert!(dir.path().join("bin/kodehub-tests").exists());
    let out = r.run_test_bundle(&tar, None).await.unwrap();
    assert_eq!(out.len(), rt.len());
    assert!(out.iter().all(|o| o.status == "pass"), "{out:?}");
    // tested many times from the same bundle (no rebuild): a second run of one test
    let one = vec![rt[0].clone()];
    let again = r.run_test_bundle(&tar, Some(&one)).await.unwrap();
    assert_eq!(again.len(), 1);

    // Python: package + vendored dependencies; scripts call pytest per test
    let pt = tests_of(&g, Language::Python);
    let (ptar, pm) = r
        .build_test_bundle(&g, Language::Python, &pt)
        .await
        .unwrap()
        .expect("builds");
    assert_eq!(pm.container_type, "python");
    let pout = r.run_test_bundle(&ptar, None).await.unwrap();
    assert!(pout.iter().all(|o| o.status == "pass"), "{pout:?}");

    // a broken helper changes the bundle key and fails exactly the tests that reach it
    edit_body(&s, "verify_password", |_| {
        "let _ = (password, hash);\nfalse".into()
    })
    .await;
    let g2 = s.load_code_graph().await.unwrap();
    let k2 = r.bundle_key(&g2, Language::Rust, &rt).unwrap();
    assert_ne!(k2, m.key);
    let (tar2, _) = r
        .build_test_bundle(&g2, Language::Rust, &rt)
        .await
        .unwrap()
        .expect("builds");
    let out2 = r.run_test_bundle(&tar2, None).await.unwrap();
    assert!(out2.iter().any(|o| o.status == "fail"));
    assert!(
        out2.iter().any(|o| o.status == "pass"),
        "unrelated tests still pass"
    );
    s.drop_db().await.unwrap();
}
