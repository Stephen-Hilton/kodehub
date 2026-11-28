//! M2 acceptance: render, build, run (real cargo / python).
mod common;

use std::time::Instant;

use common::*;
use kodehub_core::model::{Kind, Language, VersionContent};
use kodehub_runner::{BuildTarget, RunRequest};
use kodehub_store::NewSymbol;
use serde_json::json;

fn rust_roots(g: &kodehub_core::CodeGraph, lang: Language) -> Vec<String> {
    g.symbols
        .values()
        .filter(|s| s.language == lang && s.kind != Kind::Test)
        .map(|s| s.id.clone())
        .collect()
}

#[tokio::test]
async fn rendering_same_closure_twice_is_byte_identical() {
    let s = seeded("render").await;
    let r = runner();
    let g1 = s.load_code_graph().await.unwrap();
    let g2 = s.load_code_graph().await.unwrap();
    for lang in [Language::Rust, Language::Python] {
        let t = BuildTarget::Lib {
            language: lang,
            roots: rust_roots(&g1, lang),
            namespaces: vec![],
        };
        let a = r.render_target(&g1, &t).unwrap();
        let b = r.render_target(&g2, &t).unwrap();
        assert_eq!(a.files, b.files);
        assert!(a.files.contains_key("render-manifest.json"));
    }
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn seed_compiles_rust_and_imports_python() {
    let s = seeded("compile").await;
    let r = runner();
    let g = s.load_code_graph().await.unwrap();
    // namespaces from the graph map to `pub mod` (Rust) / sub-module (Python) re-exports
    let namespaces: Vec<kodehub_render::NamespaceSpec> = s
        .namespaces()
        .await
        .unwrap()
        .into_iter()
        .map(|(name, exposes)| kodehub_render::NamespaceSpec { name, exposes })
        .collect();
    for lang in [Language::Rust, Language::Python] {
        let t = BuildTarget::Lib {
            language: lang,
            roots: rust_roots(&g, lang),
            namespaces: namespaces.clone(),
        };
        let rendered = r.render_target(&g, &t).unwrap();
        match lang {
            Language::Rust => assert!(
                rendered.files["src/lib.rs"].contains("pub mod auth_sdk {\n    pub use super::hash_password as hash_new_password;\n    pub use super::verify_credentials as check_login;\n}"),
                "{}", rendered.files["src/lib.rs"]
            ),
            Language::Python => assert!(rendered.files["kodehub_gen/auth_sdk.py"].contains("from . import login as sign_in")),
        }
        let b = r.build(&g, &t).await.unwrap();
        assert!(b.ok, "{lang:?} build failed:\n{}", b.log);
        assert!(!b.cache_hit);
        let again = r.build(&g, &t).await.unwrap();
        assert!(again.cache_hit && again.ok);
    }
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn second_run_is_a_cache_hit_under_one_second_and_helpers_rebuild_only_dependents() {
    let s = seeded("runnow").await;
    let r = runner();
    let g = s.load_code_graph().await.unwrap();
    let req = |sym: &str, args| RunRequest {
        symbol: s.db.pkey(sym).unwrap(),
        args,
        environment: "dev".into(),
        project_version: None,
    };
    let store_args = json!({"store": {"users": {}}, "username": "bob", "password": "x"});

    let first = r
        .run_now(&g, &req("verify_credentials", store_args.clone()), None)
        .await
        .unwrap();
    assert!(first.ok, "{first:?}");
    assert_eq!(first.result, json!("invalid_credentials"));
    assert!(!first.cache_hit);

    let t = Instant::now();
    let second = r
        .run_now(&g, &req("verify_credentials", store_args.clone()), None)
        .await
        .unwrap();
    let elapsed = t.elapsed();
    assert!(second.cache_hit, "second run must not compile");
    assert!(second.ok);
    assert!(elapsed.as_millis() < 1000, "second run took {elapsed:?}");

    // a real hash round-trip through two runs
    let h = r
        .run_now(&g, &req("hash_password", json!({"password": "pw"})), None)
        .await
        .unwrap();
    assert!(h.ok);
    let hash = h.result.as_str().unwrap().to_string();
    assert!(hash.starts_with("$argon2id$"));
    let ok = r
        .run_now(
            &g,
            &req(
                "verify_credentials",
                json!({
        "store": {"users": {"alice": {"username": "alice", "password_hash": hash}}},
        "username": "alice", "password": "pw"}),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(ok.result, json!({"authenticated": {"username": "alice"}}));

    // change a helper used by verify_credentials but not by hash_password
    edit_body(&s, "verify_password", |b| format!("// helper changed\n{b}")).await;
    let g2 = s.load_code_graph().await.unwrap();
    let dependent = r
        .run_now(&g2, &req("verify_credentials", store_args.clone()), None)
        .await
        .unwrap();
    assert!(!dependent.cache_hit, "dependent must rebuild");
    let unrelated = r
        .run_now(&g2, &req("hash_password", json!({"password": "pw"})), None)
        .await
        .unwrap();
    assert!(unrelated.cache_hit, "non-dependent must not rebuild");

    // python run-now
    let py = r
        .run_now(
            &g2,
            &req(
                "py-login",
                json!({"username": "alice", "password": "wonderland"}),
            ),
            None,
        )
        .await
        .unwrap();
    assert!(py.ok, "{py:?}");
    assert_eq!(py.result, json!({"ok": true, "username": "alice"}));
    r.record_run(&s, &py).await.unwrap();
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn runaway_functions_are_killed_after_ten_seconds() {
    let s = seeded("timeout").await;
    let r = runner();
    s.create_symbol(
        &NewSymbol {
            key: Some(s.db.pkey("spin").unwrap()),
            display_name: "spin".into(),
            kind: Kind::Function,
            language: Language::Python,
            visibility: Default::default(),
            tags: vec![],
            description: None,
            parent: Some(s.db.qid("components/password-hashing").unwrap()),
        },
        &VersionContent {
            kind: Kind::Function,
            language: Language::Python,
            signature: "() -> int".into(),
            body: "while True:\n    pass".into(),
            attributes: json!({}),
        },
        "test",
    )
    .await
    .unwrap();
    let g = s.load_code_graph().await.unwrap();
    let t = Instant::now();
    let out = r
        .run_now(
            &g,
            &RunRequest {
                symbol: s.db.pkey("spin").unwrap(),
                args: json!({}),
                environment: "dev".into(),
                project_version: None,
            },
            None,
        )
        .await
        .unwrap();
    let el = t.elapsed().as_secs_f32();
    assert!(out.timed_out, "{out:?}");
    assert!(!out.ok);
    assert!((9.5..15.0).contains(&el), "took {el}s");
    s.drop_db().await.unwrap();
}

#[tokio::test]
async fn methods_and_unsupported_params_are_not_runnable() {
    let s = seeded("notrun").await;
    let r = runner();
    let g = s.load_code_graph().await.unwrap();
    let e = r
        .run_now(
            &g,
            &RunRequest {
                symbol: s.db.pkey("UserStore-find_user").unwrap(),
                args: json!({}),
                environment: "dev".into(),
                project_version: None,
            },
            None,
        )
        .await;
    assert!(e.is_err());
    assert!(e.unwrap_err().to_string().contains("method"));
    s.drop_db().await.unwrap();
}
