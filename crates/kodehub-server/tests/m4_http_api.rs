//! M4 acceptance: the full seed scenario driven purely through HTTP, plus events and MCP.
mod common;

use common::*;
use futures::StreamExt;
use serde_json::{json, Value};

fn statuses(cov: &Value) -> Vec<(String, String)> {
    cov["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["key"].as_str().unwrap().to_string(),
                r["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn full_seed_scenario_over_http() {
    let t = TestServer::start("http").await;

    let h = t.ok("GET", "/health", None).await;
    assert_eq!(h["status"], "ok");
    assert_eq!(h["db"]["connected"], true);

    // events socket
    let ws_url = t.url.replace("http://", "ws://") + "/api/events?token=" + &t.pat;
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    let hello = ws.next().await.unwrap().unwrap();
    assert!(hello.to_string().contains("hello"));

    // seed
    let r = t
        .ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    assert!(r["symbols_created"].as_u64().unwrap() > 10);
    let r2 = t
        .ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    assert_eq!(r2["versions_created"], 0);
    let ev = ws.next().await.unwrap().unwrap().to_string();
    assert!(ev.contains("node.changed"), "{ev}");

    // generic graph access
    let n = t
        .ok("GET", "/api/node?id=code/verify_credentials", None)
        .await;
    assert_eq!(n["display_name"], "verify_credentials");
    // code and tests are separate collections (docs/ontology.md)
    let mut py = 0;
    for coll in ["code", "tests"] {
        let q = t
            .ok(
                "POST",
                "/api/query",
                Some(json!({"collection": coll, "filter": {"language": "python"}})),
            )
            .await;
        py += q.as_array().unwrap().len();
    }
    assert_eq!(py, 7);
    let (s, e) = t
        .req("POST", "/api/query", Some(json!({"collection": "symbols"})))
        .await;
    assert!(s == 400 || s == 404, "v0.1 collections are gone: {s} {e}");
    let tr = t.ok("POST", "/api/traverse", Some(json!({"start": "code/verify_credentials", "direction": "outbound", "edges": ["calls"], "depth": 2}))).await;
    assert_eq!(tr["vertices"].as_array().unwrap().len(), 2);
    let (s, _) = t.req("GET", "/api/node?id=code/nope", None).await;
    assert_eq!(s, 404);

    // requirements and coverage
    let reqs = t.ok("GET", "/api/requirements", None).await;
    assert_eq!(reqs["requirements"].as_array().unwrap().len(), 2);
    let cov = t.ok("GET", "/api/coverage", None).await;
    assert!(
        statuses(&cov).iter().all(|(_, s)| s == "at_risk"),
        "untested at first"
    );

    let tests = t
        .ok(
            "POST",
            "/api/tests/run",
            Some(json!({"scope": {"kind": "all"}})),
        )
        .await;
    assert_eq!(tests["summary"]["failed"], 0, "{tests}");
    let cov = t.ok("GET", "/api/coverage", None).await;
    assert!(
        statuses(&cov).iter().all(|(_, s)| s == "pass"),
        "{:?}",
        statuses(&cov)
    );
    assert_eq!(cov["tags"][0]["tag"], "secure-authentication");
    assert_eq!(cov["tags"][0]["status"], "pass");

    // run now (cold, then warm)
    let args = json!({"store": {"users": {}}, "username": "", "password": "x"});
    let run = t
        .ok(
            "POST",
            "/api/run",
            Some(json!({"symbol": "verify_credentials", "args": args})),
        )
        .await;
    assert_eq!(run["result"], "invalid_credentials");
    let run2 = t
        .ok(
            "POST",
            "/api/run",
            Some(json!({"symbol": "verify_credentials", "args": args})),
        )
        .await;
    assert_eq!(run2["cache_hit"], true);
    let runs = t
        .ok("GET", "/api/runs?symbol=verify_credentials", None)
        .await;
    assert_eq!(runs.as_array().unwrap().len(), 2);
    let (s, e) = t
        .req(
            "POST",
            "/api/run",
            Some(json!({"symbol": "UserStore-find_user", "args": {}})),
        )
        .await;
    assert_eq!(s, 400, "{e}");
    let (s, _) = t
        .req(
            "POST",
            "/api/run",
            Some(json!({"symbol": "verify_credentials", "environment": "prod"})),
        )
        .await;
    assert_eq!(s, 400, "nothing deployed to prod yet");

    // traceability
    let imp = t.ok("GET", "/api/impact/hash_password", None).await;
    assert_eq!(imp["requirements"].as_array().unwrap().len(), 2);
    assert_eq!(imp["tags"], json!(["secure-authentication"]));
    assert_eq!(t.ok("GET", "/api/orphans", None).await, json!([]));
    assert_eq!(
        t.ok("GET", "/api/duplicates", None)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // symbol detail, map
    let d = t.ok("GET", "/api/symbols/verify_credentials", None).await;
    assert_eq!(d["form"]["runnable"], true);
    assert_eq!(d["form"]["params"][0]["spec"]["type"], "struct");
    assert_eq!(d["health"], "pass");
    // R1 applies to it directly, R2 through the context that holds it
    assert_eq!(
        d["requirements"].as_array().unwrap().len(),
        2,
        "{}",
        d["requirements"]
    );
    assert_eq!(d["tags"], json!(["secure-authentication"]));
    assert_eq!(d["decisions"].as_array().unwrap().len(), 1);
    assert_eq!(d["decisions"][0]["kind"], "decision");
    assert_eq!(d["node"], t.q("code/verify_credentials"));
    let map = t.ok("GET", "/api/map", None).await;
    assert!(map["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["id"] == t.q("contexts/authentication") && n["health"] == "pass"));
    assert_eq!(map["project_version"]["current"], true);
    assert!(map["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["id"] == t.q("tests/test_valid_login_succeeds") && n["level"] == "symbol"));
    assert!(map["edges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "tested_with"));
    assert!(map["mission"]["statement"]
        .as_str()
        .unwrap()
        .contains("never leaks"));

    // requirement edit with history
    let edit = t.uok("POST", "/api/requirements/r1-generic-failure/edge_cases",
        Some(json!({"description": "Whitespace-only username", "input": {"username": "  "}, "expected": "invalid_credentials"}))).await;
    assert_eq!(edit["changed"], true);
    assert_eq!(edit["requirement"]["revision"], 2);
    let hist = t
        .ok("GET", "/api/requirements/r1-generic-failure/history", None)
        .await;
    // every requirement version, newest first (each edit is a new immutable version)
    assert_eq!(hist.as_array().unwrap().len(), 2);
    assert_eq!(hist[0]["revision"], 2);
    assert_eq!(hist[1]["revision"], 1);
    let cov = t
        .ok("GET", "/api/coverage?requirement=r1-generic-failure", None)
        .await;
    assert_eq!(cov["requirements"][0]["status"], "at_risk");

    // decision
    let dec = t.ok("POST", "/api/decisions", Some(json!({"title": "Trim usernames", "choice": "no", "rationale": "keep simple", "justifies": ["code/verify_credentials"]}))).await;
    assert!(dec["_key"].is_string());
    assert_eq!(dec["kind"], "decision");
    let decs = t.ok("GET", "/api/decisions", None).await;
    assert_eq!(decs.as_array().unwrap().len(), 2);
    let reqs = t.ok("GET", "/api/requirements", None).await;
    assert_eq!(
        reqs["requirements"].as_array().unwrap().len(),
        2,
        "decisions are not listed as requirements"
    );

    // a work item's proposal, by hand (D-058)
    let cs = t.ok("POST", "/api/proposals", Some(json!({"title": "Comment verify_credentials", "requirements": ["r1-generic-failure"]}))).await;
    let k = cs["_key"].as_str().unwrap().to_string();
    assert_eq!(
        cs["work_item"],
        json!(k),
        "the proposal is keyed by its work item"
    );
    let wi = t.ok("GET", &format!("/api/work-items/{k}"), None).await;
    assert_eq!(wi["agent_type"], "coding");
    assert!(
        wi["project_version"]
            .as_str()
            .unwrap()
            .contains("project_versions/"),
        "{wi}"
    );
    let open = t.ok("GET", "/api/proposals?open=true", None).await;
    assert_eq!(open.as_array().unwrap().len(), 1);
    t.ok("POST", &format!("/api/work-items/{k}/proposal/plan"), Some(json!({"plan": {"summary": "doc comment", "changes": [{"symbol": "verify_credentials", "action": "modify"}]}}))).await;
    let (s, _) = t
        .req(
            "POST",
            &format!("/api/work-items/{k}/proposal/accept"),
            None,
        )
        .await;
    assert_eq!(s, 403, "a remote agent never accepts (D-061)");
    let (s, _) = t
        .ui(
            "POST",
            &format!("/api/work-items/{k}/proposal/accept"),
            None,
        )
        .await;
    assert_eq!(s, 400, "cannot accept before ready");
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/approve"),
        None,
    )
    .await;
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/transition"),
        Some(json!({"to": "building"})),
    )
    .await;
    let cur = t.ok("GET", "/api/symbols/verify_credentials", None).await;
    let body = format!(
        "// Every failure is the same generic result.\n{}",
        cur["version"]["body"].as_str().unwrap()
    );
    let p = t.ok("POST", &format!("/api/work-items/{k}/proposal/versions"), Some(json!({"symbol": "verify_credentials", "signature": cur["version"]["signature"], "body": body, "by": "coder"}))).await;
    assert_eq!(p["new"], false);
    let still = t.ok("GET", "/api/symbols/verify_credentials", None).await;
    assert_eq!(
        still["version"]["_key"], cur["version"]["_key"],
        "nothing changes in the project version until acceptance"
    );
    t.uok(
        "POST",
        &format!("/api/work-items/{k}/proposal/transition"),
        Some(json!({"to": "reviewing"})),
    )
    .await;
    // a remote agent cannot review at all (D-061); without an attested review nothing is ready
    let (s403, _) = t
        .req(
            "POST",
            &format!("/api/work-items/{k}/proposal/reviews"),
            Some(json!({"findings": [], "summary": "lgtm (agent)"})),
        )
        .await;
    assert_eq!(s403, 403);
    let tr = t
        .ok("POST", &format!("/api/work-items/{k}/proposal/test"), None)
        .await;
    assert_eq!(tr["passed"], true, "{tr}");
    assert_eq!(
        tr["state"], "testing",
        "unattested review must not reach ready"
    );
    let checks = t
        .ok("GET", &format!("/api/work-items/{k}/proposal/checks"), None)
        .await;
    assert_eq!(checks["checks"]["review_attested"], false);
    // engine-owned states can't be set directly
    let (s, _) = t
        .ui(
            "POST",
            &format!("/api/work-items/{k}/proposal/transition"),
            Some(json!({"to": "accepted"})),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = t
        .ui(
            "POST",
            &format!("/api/work-items/{k}/proposal/transition"),
            Some(json!({"to": "ready"})),
        )
        .await;
    assert_eq!(s, 400);
    // R1 was edited earlier (new edge case): a proposal listing it must bring a verifying test
    let checks = t
        .ok("GET", &format!("/api/work-items/{k}/proposal/checks"), None)
        .await;
    assert_eq!(checks["checks"]["requirements_covered"], false, "{checks}");
    t.ok("POST", &format!("/api/work-items/{k}/proposal/versions"), Some(json!({
        "display_name": "test_whitespace_username_is_invalid", "kind": "test", "language": "rust", "signature": "",
        "body": "let mut store = UserStore::new();\nstore.add_user(\"alice\", \"x\");\nassert_eq!(verify_credentials(&store, \"  \", \"x\"), AuthResult::InvalidCredentials);",
        "verifies": ["r1-generic-failure"], "tests_for": ["verify_credentials"], "component": "credential-verification", "by": "tester"}))).await;
    // the human reviews in the app → attested → tests move it to ready
    let (s, _) = t.ui("POST", &format!("/api/work-items/{k}/proposal/reviews"),
        Some(json!({"findings": [{"severity": "minor", "location": "verify_credentials", "finding": "fine"}], "summary": "ok", "by": "human"}))).await;
    assert_eq!(s, 200);
    let tr = t
        .ok("POST", &format!("/api/work-items/{k}/proposal/test"), None)
        .await;
    assert_eq!(tr["state"], "ready", "{tr}");
    let checks = t
        .ok("GET", &format!("/api/work-items/{k}/proposal/checks"), None)
        .await;
    assert_eq!(checks["ok"], true, "{checks}");
    let m = t
        .uok(
            "POST",
            &format!("/api/work-items/{k}/proposal/accept"),
            None,
        )
        .await;
    assert_eq!(m["accepted"], true);
    assert_eq!(m["proposal"]["state"], "accepted");
    let wi = t.ok("GET", &format!("/api/work-items/{k}"), None).await;
    assert_eq!(wi["state"], "done", "accepting settles the work item: {wi}");
    // the new test is a node of its own, placed under its component
    let nt = t
        .ok(
            "GET",
            "/api/symbols/test_whitespace_username_is_invalid",
            None,
        )
        .await;
    assert_eq!(nt["node"], t.q("tests/test_whitespace_username_is_invalid"));
    let after = t.ok("GET", "/api/symbols/verify_credentials", None).await;
    assert_ne!(after["version"]["_key"], cur["version"]["_key"]);
    assert_eq!(after["history"].as_array().unwrap().len(), 2);
    let cov = t
        .ok("GET", "/api/coverage?requirement=r1-generic-failure", None)
        .await;
    assert_eq!(
        cov["requirements"][0]["status"], "pass",
        "accepted with warm test cache: {cov}"
    );
    let inv = t.ok("GET", "/api/invariants", None).await;
    assert_eq!(inv["ok"], true);
    let tl = t.ok("GET", "/api/timeline", None).await;
    assert!(tl["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "accept"));

    // immutability through HTTP
    let v = after["version"]["_key"].as_str().unwrap();
    let (s, _) = t
        .ui(
            "PATCH",
            &format!("/api/nodes/code_versions/{v}"),
            Some(json!({"body": "x"})),
        )
        .await;
    assert_eq!(s, 409);
    let (s, _) = t
        .ui(
            "POST",
            "/api/edges/has_version",
            Some(json!({"from": "code/hash_password", "to": format!("code_versions/{v}")})),
        )
        .await;
    assert_eq!(s, 400);

    // browser origins: only the app's own; terminal needs one; foreign hosts are refused
    let (s, _) = t.ui("GET", "/api/requirements", None).await;
    assert_eq!(s, 200);
    let resp = reqwest::Client::new()
        .get(format!("{}/api/requirements", t.url))
        .header("Origin", "http://localhost:9999")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let resp = reqwest::Client::new()
        .get(format!("{}/api/requirements", t.url))
        .header("Host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    // no token: 401
    let (s, _) = t.req_as(None, "GET", "/api/requirements", None).await;
    assert_eq!(s, 401);
    let (s, _) = t
        .req_as(Some("khp_forged"), "GET", "/api/requirements", None)
        .await;
    assert_eq!(s, 401);
    // an agent's token plus a forged Origin and a forged UI header is not enough for
    // human-only actions: only a human's session is
    // (a remote agent may edit requirements since D-061; the philosophy stays a human's)
    let forged = reqwest::Client::new()
        .put(format!("{}/api/philosophy", t.url))
        .header("Origin", "http://localhost:1420")
        .header("x-kodehub-ui", t.state.ui_token.as_str())
        .bearer_auth(&t.pat)
        .json(&json!({"text": "an agent rewrites the philosophy"}))
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status().as_u16(), 403);
    let forged = reqwest::Client::new()
        .post(format!("{}/api/deployments/x/approve", t.url))
        .header("Origin", "http://localhost:1420")
        .header("x-kodehub-ui", t.state.ui_token.as_str())
        .bearer_auth(&t.pat)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status().as_u16(), 403);
    // code / trace edges can't be changed outside a proposal
    let (s, _) = t.ui("POST", "/api/edges/verified_by", Some(json!({"from": "requirements/r2-hashed-storage", "to": "tests/test_unknown_user_is_invalid"}))).await;
    assert_eq!(s, 400);
    let (s, _) = t.ui("DELETE", "/api/edges/tested_with?from=code/verify_credentials&to=tests/test_valid_login_succeeds", None).await;
    assert_eq!(s, 400);
    let ws_url = t.url.replace("http://", "ws://") + "/api/terminal";
    assert!(
        tokio_tungstenite::connect_async(&ws_url).await.is_err(),
        "terminal requires the app's origin"
    );

    // settings: only env var names, never values
    let st = t.ok("GET", "/api/settings", None).await;
    assert!(st["agents"]["available_token_vars"].is_array());
    let (s, _) = t
        .ui(
            "PATCH",
            "/api/settings/agents",
            Some(json!({"token_env": "ANTHROPIC_API_KEY"})),
        )
        .await;
    assert_eq!(s, 400);
    t.stop().await;
}

#[tokio::test]
async fn mcp_stdio_lists_requirements_and_runs_coverage() {
    let t = TestServer::start("mcp").await;
    t.ok("POST", "/api/seed", Some(json!({"path": seed_path()})))
        .await;
    let bin = repo().join("target/debug/kodehub");
    assert!(bin.exists(), "build the CLI first (cargo build --bins)");
    let mut child = tokio::process::Command::new(&bin)
        .args(["--server", &t.url, "mcp"])
        .env("KODEHUB_TOKEN", &t.pat)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
    let msgs = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "list_requirements", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "coverage", "arguments": {}}}),
    ];
    for m in &msgs {
        stdin.write_all(format!("{m}\n").as_bytes()).await.unwrap();
    }
    stdin.flush().await.unwrap();
    let mut responses = Vec::new();
    while responses.len() < 4 {
        let line = out.next_line().await.unwrap().unwrap();
        responses.push(serde_json::from_str::<Value>(&line).unwrap());
    }
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-06-18");
    let tools = responses[1]["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "coverage"));
    assert!(tools.iter().any(|t| t["name"] == "propose_version"));
    let text = responses[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("R1 — Generic failure"));
    let text = responses[3]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("\"requirements\""));
    assert_eq!(responses[3]["result"]["isError"], false);
    child.kill().await.ok();
    t.stop().await;
}
