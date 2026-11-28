//! Remote MCP (spec §13A.2): the MCP service over HTTP behind OAuth 2.1.
//!
//! - `/.well-known/oauth-protected-resource` (RFC 9728) and `/.well-known/oauth-authorization-server`
//!   (RFC 8414) for discovery;
//! - `POST /oauth/register` (RFC 7591), `GET|POST /oauth/authorize` (a Kodehub sign-in and consent
//!   page; PKCE S256 required), `POST /oauth/token` (authorization code and refresh grants);
//! - `POST /mcp`: JSON-RPC, the same tools as `kodehub mcp`, each call made against the API with
//!   the caller's own token — the MCP service adds no logic and no security of its own.

use std::collections::BTreeMap;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::AppState;

fn base(headers: &HeaderMap) -> String {
    if let Ok(u) = std::env::var("KODEHUB_PUBLIC_URL") {
        if !u.is_empty() {
            return u.trim_end_matches('/').to_string();
        }
    }
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:7878");
    format!("http://{host}")
}

pub async fn protected_resource(headers: HeaderMap) -> Json<Value> {
    let b = base(&headers);
    Json(
        json!({"resource": format!("{b}/mcp"), "authorization_servers": [b],
                "bearer_methods_supported": ["header"], "resource_name": "Kodehub"}),
    )
}

pub async fn authorization_server(headers: HeaderMap) -> Json<Value> {
    let b = base(&headers);
    Json(json!({
        "issuer": b,
        "authorization_endpoint": format!("{b}/oauth/authorize"),
        "token_endpoint": format!("{b}/oauth/token"),
        "registration_endpoint": format!("{b}/oauth/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": ["kodehub"],
    }))
}

fn oauth_error(status: StatusCode, err: &str, desc: &str) -> Response {
    (
        status,
        Json(json!({"error": err, "error_description": desc})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct RegisterBody {
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
}

pub async fn register(State(s): State<AppState>, Json(b): Json<RegisterBody>) -> Response {
    match s
        .store
        .oauth_register(
            b.client_name.as_deref().unwrap_or("MCP client"),
            &b.redirect_uris,
        )
        .await
    {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            &e.to_string(),
        ),
    }
}

#[derive(Deserialize, Clone)]
pub struct AuthorizeQ {
    #[serde(default)]
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub code_challenge: String,
    #[serde(default)]
    pub code_challenge_method: String,
    #[serde(default)]
    pub scope: Option<String>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn consent_page(client: &str, q: &AuthorizeQ, error: Option<&str>) -> Html<String> {
    let hidden: String = [
        ("response_type", q.response_type.as_str()),
        ("client_id", q.client_id.as_str()),
        ("redirect_uri", q.redirect_uri.as_str()),
        ("state", q.state.as_deref().unwrap_or("")),
        ("code_challenge", q.code_challenge.as_str()),
        ("code_challenge_method", q.code_challenge_method.as_str()),
    ]
    .iter()
    .map(|(k, v)| format!("<input type=\"hidden\" name=\"{k}\" value=\"{}\">", esc(v)))
    .collect();
    Html(format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Connect to Kodehub</title><style>
:root{{color-scheme:dark}}body{{margin:0;min-height:100vh;display:grid;place-items:center;background:#090c13;color:#e7eaf3;font:15px -apple-system,Inter,sans-serif;padding:16px}}
form{{width:min(400px,100%);background:#131a27;border:1px solid rgba(148,163,184,.2);border-radius:12px;padding:24px;display:flex;flex-direction:column;gap:12px}}
input[type=text],input[type=password]{{background:#0e131d;border:1px solid rgba(148,163,184,.25);color:inherit;border-radius:7px;padding:9px 10px;font:inherit}}
button{{background:#8b9bff;color:#0b0f1a;border:0;border-radius:7px;padding:10px;font-weight:650;cursor:pointer}}.muted{{color:#8e97ab;font-size:13px}}.err{{color:#f87171}}</style></head>
<body><form method="post" action="/oauth/authorize">{hidden}
<b style="font-size:18px">Connect {client} to Kodehub</b>
<p class="muted">{client} will act as its own agent user on your behalf: it can do what you can in your projects, its actions are audited as an agent's, and you can revoke it at any time in Settings.</p>
{err}<input type="text" name="username" placeholder="Username" autocomplete="username" required>
<input type="password" name="password" placeholder="Password" autocomplete="current-password" required>
<button name="approve" value="yes">Sign in and allow</button></form></body></html>"#,
        client = esc(client),
        err = error
            .map(|e| format!("<p class=\"err\">{}</p>", esc(e)))
            .unwrap_or_default(),
    ))
}

pub async fn authorize_page(State(s): State<AppState>, Query(q): Query<AuthorizeQ>) -> Response {
    if q.response_type != "code" {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_response_type",
            "response_type must be code",
        );
    }
    let client = match s.store.oauth_client(&q.client_id).await {
        Ok(c) => c,
        Err(_) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_client",
                "unknown client_id",
            )
        }
    };
    if !client["redirect_uris"]
        .as_array()
        .is_some_and(|a| a.iter().any(|u| u == q.redirect_uri.as_str()))
    {
        // never redirect to an unregistered URI
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "redirect_uri is not registered",
        );
    }
    consent_page(
        client["client_name"].as_str().unwrap_or("An assistant"),
        &q,
        None,
    )
    .into_response()
}

pub async fn authorize_submit(
    State(s): State<AppState>,
    Form(f): Form<BTreeMap<String, String>>,
) -> Response {
    let get = |k: &str| f.get(k).cloned().unwrap_or_default();
    let q = AuthorizeQ {
        response_type: get("response_type"),
        client_id: get("client_id"),
        redirect_uri: get("redirect_uri"),
        state: f.get("state").cloned().filter(|x| !x.is_empty()),
        code_challenge: get("code_challenge"),
        code_challenge_method: get("code_challenge_method"),
        scope: None,
    };
    let client = match s.store.oauth_client(&q.client_id).await {
        Ok(c) => c,
        Err(_) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_client",
                "unknown client_id",
            )
        }
    };
    let name = client["client_name"]
        .as_str()
        .unwrap_or("An assistant")
        .to_string();
    if get("approve") != "yes" {
        return consent_page(&name, &q, Some("Not approved.")).into_response();
    }
    let user = match s.store.login(&get("username"), &get("password")).await {
        Ok((session, u)) => {
            let _ = s.store.logout(&session).await;
            u["key"].as_str().unwrap_or_default().to_string()
        }
        Err(_) => {
            return consent_page(&name, &q, Some("Wrong username or password.")).into_response()
        }
    };
    match s
        .store
        .oauth_code(
            &q.client_id,
            &user,
            &q.redirect_uri,
            &q.code_challenge,
            &q.code_challenge_method,
        )
        .await
    {
        Ok(code) => {
            let mut url = url_with(&q.redirect_uri, "code", &code);
            if let Some(st) = &q.state {
                url = url_with(&url, "state", st);
            }
            let _ = s
                .store
                .audit(
                    None,
                    None,
                    "OAUTH authorize",
                    302,
                    json!({"client": name, "user": user}),
                )
                .await;
            Redirect::to(&url).into_response()
        }
        Err(e) => oauth_error(StatusCode::BAD_REQUEST, "invalid_request", &e.to_string()),
    }
}

fn url_with(base: &str, k: &str, v: &str) -> String {
    let enc: String = v
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    format!(
        "{base}{}{k}={enc}",
        if base.contains('?') { '&' } else { '?' }
    )
}

pub async fn token(State(s): State<AppState>, Form(f): Form<BTreeMap<String, String>>) -> Response {
    let get = |k: &str| f.get(k).cloned().unwrap_or_default();
    let res = match get("grant_type").as_str() {
        "authorization_code" => {
            s.store
                .oauth_exchange(
                    &get("code"),
                    &get("client_id"),
                    &get("redirect_uri"),
                    &get("code_verifier"),
                )
                .await
        }
        "refresh_token" => {
            s.store
                .oauth_refresh(&get("refresh_token"), &get("client_id"))
                .await
        }
        _ => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "use authorization_code or refresh_token",
            )
        }
    };
    match res {
        Ok(v) => {
            let mut r = Json(v).into_response();
            r.headers_mut()
                .insert("cache-control", HeaderValue::from_static("no-store"));
            r
        }
        Err(_) => oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "the code, verifier or refresh token is not valid",
        ),
    }
}

/// MCP over HTTP: the caller's bearer token is checked here and then used, unchanged, for every
/// API call the tools make — permissions, scoping and audit are the API's.
pub async fn mcp(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let b = base(&headers);
    let unauthorized = || {
        let mut r = (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_token"})),
        )
            .into_response();
        if let Ok(v) = HeaderValue::from_str(&format!(
            "Bearer resource_metadata=\"{b}/.well-known/oauth-protected-resource\""
        )) {
            r.headers_mut()
                .insert(axum::http::header::WWW_AUTHENTICATE, v);
        }
        r
    };
    let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string())
    else {
        return unauthorized();
    };
    if s.store.authenticate(&token).await.is_err() {
        return unauthorized();
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return Json(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}}))
                .into_response()
        }
    };
    // tools call the API on this server (loopback) as the caller
    let self_url = std::env::var("KODEHUB_INTERNAL_URL").unwrap_or_else(|_| b.clone());
    let mut client = kodehub_cli::Client::new(&self_url).with_token(Some(token));
    client.project = headers
        .get("x-kodehub-project")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let opts = kodehub_cli::mcp::McpOptions {
        read_only: false,
        allow: None,
        work_item: None,
    };
    let reply = match &msg {
        Value::Array(batch) => {
            let mut out = Vec::new();
            for m in batch {
                if let Some(r) = kodehub_cli::mcp::handle(&client, &opts, m).await {
                    out.push(r);
                }
            }
            Some(Value::Array(out))
        }
        m => kodehub_cli::mcp::handle(&client, &opts, m).await,
    };
    match reply {
        Some(v) => Json(v).into_response(),
        // notifications only
        None => StatusCode::ACCEPTED.into_response(),
    }
}
