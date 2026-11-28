//! S11 acceptance: remote MCP over HTTP behind OAuth 2.1 (discovery, dynamic registration,
//! consent, PKCE, refresh rotation, revocation) acting as an agent user; speech transcription
//! through a transcription job (a stub command here; the cluster smoke test runs Whisper).
mod common;

use common::*;
use serde_json::{json, Value};

fn pkce() -> (String, String) {
    let verifier = "kodehub-test-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
    (verifier.to_string(), kodehub_store::oauth::s256(verifier))
}

#[tokio::test]
async fn remote_mcp_with_oauth_and_transcription() {
    let t = TestServer::start("s11").await;
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // unauthenticated MCP: 401 pointing at the protected-resource metadata
    let r = http
        .post(format!("{}/mcp", t.url))
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    let www = r.headers()["www-authenticate"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        www.contains("/.well-known/oauth-protected-resource"),
        "{www}"
    );
    let pr: Value = http
        .get(format!("{}/.well-known/oauth-protected-resource", t.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(pr["resource"].as_str().unwrap().ends_with("/mcp"));
    let asm: Value = http
        .get(format!("{}/.well-known/oauth-authorization-server", t.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(asm["code_challenge_methods_supported"], json!(["S256"]));

    // dynamic client registration (an http redirect off loopback is refused)
    let bad = http
        .post(format!("{}/oauth/register", t.url))
        .json(&json!({"client_name": "x", "redirect_uris": ["http://evil.example/cb"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status().as_u16(), 400);
    let reg: Value = http
        .post(format!("{}/oauth/register", t.url))
        .json(
            &json!({"client_name": "Claude", "redirect_uris": ["http://127.0.0.1:5555/callback"]}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let client_id = reg["client_id"].as_str().unwrap().to_string();

    // consent page, then sign in and allow
    let (verifier, challenge) = pkce();
    let q = format!(
        "response_type=code&client_id={client_id}&redirect_uri=http%3A%2F%2F127.0.0.1%3A5555%2Fcallback&state=xyz&code_challenge={challenge}&code_challenge_method=S256"
    );
    let page = http
        .get(format!("{}/oauth/authorize?{q}", t.url))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Connect Claude to Kodehub") && page.contains("its own agent user"));
    let form = |pw: &str| {
        vec![
            ("response_type", "code".to_string()),
            ("client_id", client_id.clone()),
            ("redirect_uri", "http://127.0.0.1:5555/callback".to_string()),
            ("state", "xyz".to_string()),
            ("code_challenge", challenge.clone()),
            ("code_challenge_method", "S256".to_string()),
            ("username", "tester".to_string()),
            ("password", pw.to_string()),
            ("approve", "yes".to_string()),
        ]
    };
    let wrong = http
        .post(format!("{}/oauth/authorize", t.url))
        .form(&form("nope-nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        wrong.status().as_u16(),
        200,
        "wrong password: the page again, no redirect"
    );
    let ok = http
        .post(format!("{}/oauth/authorize", t.url))
        .form(&form("password123"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 303);
    let loc = ok.headers()["location"].to_str().unwrap().to_string();
    assert!(
        loc.starts_with("http://127.0.0.1:5555/callback?code=") && loc.ends_with("&state=xyz"),
        "{loc}"
    );
    let code = loc
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();

    // token: the verifier must match; the code is single-use
    let tok = |code: &str, v: &str| {
        vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("client_id", client_id.clone()),
            ("redirect_uri", "http://127.0.0.1:5555/callback".to_string()),
            ("code_verifier", v.to_string()),
        ]
    };
    let r = http
        .post(format!("{}/oauth/token", t.url))
        .form(&tok(
            &code,
            "wrong-verifier-wrong-verifier-wrong-verifier-00",
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    let pair: Value = http
        .post(format!("{}/oauth/token", t.url))
        .form(&tok(&code, &verifier))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let access = pair["access_token"].as_str().unwrap().to_string();
    let refresh = pair["refresh_token"].as_str().unwrap().to_string();
    let r = http
        .post(format!("{}/oauth/token", t.url))
        .form(&tok(&code, &verifier))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400, "codes are single-use");

    // MCP over HTTP: the assistant is an agent user acting for the human
    let mcp = |tok: String, body: Value| {
        let http = http.clone();
        let url = format!("{}/mcp", t.url);
        async move {
            http.post(url)
                .bearer_auth(tok)
                .json(&body)
                .send()
                .await
                .unwrap()
        }
    };
    let init: Value = mcp(access.clone(), json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}))
        .await.json().await.unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "kodehub");
    let list: Value = mcp(access.clone(), json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "list_requirements", "arguments": {}}}))
        .await.json().await.unwrap();
    assert_eq!(list["result"]["isError"], false, "{list}");
    assert!(list["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("R1 — Generic failure"));
    let me = t.store.authenticate(&access).await.unwrap();
    assert!(!me.is_human());
    assert_eq!(me.on_behalf_of().as_deref(), Some("users/tester"));
    let audit = t.uok("GET", "/api/audit?class=external_agent", None).await;
    assert!(audit
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["actor_class"] == "external_agent"));

    // refresh rotates; the old refresh token stops working; refresh tokens are not bearer tokens
    assert!(t.store.authenticate(&refresh).await.is_err());
    let rf = |r: &str| {
        vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", r.to_string()),
            ("client_id", client_id.clone()),
        ]
    };
    let pair2: Value = http
        .post(format!("{}/oauth/token", t.url))
        .form(&rf(&refresh))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let access2 = pair2["access_token"].as_str().unwrap().to_string();
    assert_eq!(
        http.post(format!("{}/oauth/token", t.url))
            .form(&rf(&refresh))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    assert_eq!(
        mcp(
            access2.clone(),
            json!({"jsonrpc": "2.0", "id": 3, "method": "ping"})
        )
        .await
        .status()
        .as_u16(),
        200
    );

    // the human revokes the connection: the next call fails
    let toks = t.uok("GET", "/api/tokens", None).await;
    for tk in toks.as_array().unwrap() {
        t.uok(
            "DELETE",
            &format!("/api/tokens/{}", tk["id"].as_str().unwrap()),
            None,
        )
        .await;
    }
    assert_eq!(
        mcp(
            access2,
            json!({"jsonrpc": "2.0", "id": 4, "method": "ping"})
        )
        .await
        .status()
        .as_u16(),
        401
    );

    // speech transcription: the audio goes through a transcription job (stub command here)
    let stub = tempfile::tempdir().unwrap();
    let script = stub.path().join("whisper.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ntest -s \"$1\" && echo \"Checkout never loses an order.\"\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("KODEHUB_TRANSCRIBE_CMD", script.display().to_string());
    std::env::set_var(
        "KODEHUB_WORKER_BIN",
        repo().join("target/debug/kodehub-worker"),
    );
    std::env::set_var("KODEHUB_HOME", &t.state.runner.home);
    let r = reqwest::Client::new()
        .post(format!("{}/api/transcribe", t.url))
        .bearer_auth(&t.session)
        .header("content-type", "audio/wav")
        .body(vec![1u8; 2048])
        .send()
        .await
        .unwrap();
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["text"], "Checkout never loses an order.", "{v}");
    t.stop().await;
}
