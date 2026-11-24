//! M8 (stretch): Python `login()` calls Rust `verify_credentials` through a generated PyO3
//! extension; changing the Rust signature marks the Python wrapper at risk.
mod common;

use common::*;
use kodehub_core::model::{Kind, Language, VersionContent};
use kodehub_runner::{RunRequest, TestScope};
use kodehub_store::version_content_from_doc;
use serde_json::json;

#[tokio::test]
async fn python_login_calls_rust_through_generated_extension() {
    let s = seeded("pyo3").await;
    let r = runner();
    // login now delegates to the Rust verify_credentials (bound via binds_to)
    s.write_version(
        "py-login",
        &VersionContent {
            kind: Kind::Function,
            language: Language::Python,
            signature: "(username: str, password: str) -> dict".into(),
            body: "users = {u: {\"username\": u, \"password_hash\": h} for u, h in demo_users().items()}\nresult = verify_credentials({\"users\": users}, username, password)\nif isinstance(result, dict) and \"authenticated\" in result:\n    return {\"ok\": True, \"username\": username}\nreturn {\"ok\": False, \"error\": \"invalid_credentials\"}".into(),
            attributes: json!({}),
        },
        "test",
    )
    .await
    .unwrap();
    s.remove_edge("calls", "symbols/py-login", "symbols/py-authenticate")
        .await
        .unwrap();
    s.insert_edge(
        "binds_to",
        "symbols/py-login",
        "symbols/verify_credentials",
        json!({}),
    )
    .await
    .unwrap();

    let g = s.load_code_graph().await.unwrap();
    // the Python closure now includes the Rust code
    let cl = g.closure(&["py-login".to_string()], false).unwrap();
    assert!(cl.contains_key("verify_credentials") && cl.contains_key("verify_password"));

    // the rendered package really goes through the generated extension
    let rendered = r
        .render_target(
            &g,
            &kodehub_runner::BuildTarget::Run {
                language: Language::Python,
                symbol: "py-login".into(),
            },
        )
        .unwrap();
    assert!(rendered.files["kodehub_gen/__init__.py"]
        .contains("from . import kodehub_ext as _kodehub_ext"));
    assert!(rendered.files["kodehub_gen/__init__.py"]
        .contains("def verify_credentials(*args, **kwargs):"));
    assert!(rendered.ext.as_ref().unwrap().files["src/lib.rs"].contains("fn kodehub_ext("));
    // python tests for login pass through the extension
    let tests = r
        .resolve_scope(&s, &g, &TestScope::Symbol("py-login".into()))
        .await
        .unwrap();
    assert_eq!(tests.len(), 4);
    let out = r
        .run_tests(&s, &g, &tests, false, None, None)
        .await
        .unwrap();
    assert!(out.iter().all(|o| o.status == "pass"), "{out:#?}");

    // run now: the Python wrapper returns the Rust result
    let run = r
        .run_now(
            &g,
            &RunRequest {
                symbol: "py-login".into(),
                args: json!({"username": "alice", "password": "wonderland"}),
                environment: "dev".into(),
                release: None,
            },
            None,
        )
        .await
        .unwrap();
    assert!(run.ok, "{run:?}");
    assert_eq!(run.result, json!({"ok": true, "username": "alice"}));
    let again = r
        .run_now(
            &g,
            &RunRequest {
                symbol: "py-login".into(),
                args: json!({"username": "alice", "password": "wonderland"}),
                environment: "dev".into(),
                release: None,
            },
            None,
        )
        .await
        .unwrap();
    assert!(again.cache_hit, "the python+extension artifact is cached");
    let bad = r
        .run_now(
            &g,
            &RunRequest {
                symbol: "py-login".into(),
                args: json!({"username": "alice", "password": "nope"}),
                environment: "dev".into(),
                release: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        bad.result,
        json!({"ok": false, "error": "invalid_credentials"})
    );

    // the binding shows up in impact analysis
    let imp = s.impact("verify_credentials").await.unwrap();
    assert!(imp["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["id"] == "symbols/py-login"));
    let before = s.node_health().await.unwrap();
    assert_eq!(before["symbols/py-login"]["health"], "pass");

    // changing the Rust signature marks the Python wrapper (and R1) at risk
    let doc = s.aql("FOR e IN @@has_version FILTER e._from == CONCAT(@pfx, 'symbols/verify_credentials') AND e.state == 'stable' RETURN DOCUMENT(e._to)", json!({})).await.unwrap();
    let mut c = version_content_from_doc(&doc[0]).unwrap();
    c.signature =
        "(store: &UserStore, username: &str, password: &str, _audit: Option<String>) -> AuthResult"
            .into();
    s.write_version("verify_credentials", &c, "test")
        .await
        .unwrap();
    let after = s.node_health().await.unwrap();
    assert_eq!(
        after["symbols/py-login"]["health"], "at_risk",
        "{}",
        after["symbols/py-login"]
    );
    let cov = s.coverage(Some("r1-generic-failure")).await.unwrap();
    assert_eq!(cov.requirements[0].status, "at_risk");
    s.drop_db().await.unwrap();
}
