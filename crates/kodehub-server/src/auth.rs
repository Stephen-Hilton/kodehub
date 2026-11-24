//! Authentication, request-scoped project selection and the audit trail (spec §6, §13A).
//!
//! Every `/api/*` call needs a bearer token (`Authorization: Bearer …`, or `?token=` for
//! WebSockets, which cannot set headers), except sign-in, registration and health. The project
//! comes from `x-kodehub-project` (or `?project=`), falling back to the caller's default; the
//! caller must hold at least `read` (GET) or `write` (anything else) on it. The handler then runs
//! with that project as the request scope, so concurrent requests never share a project.
//!
//! Human-only actions keep their v0.05 check (`require_ui`): this layer strips any
//! client-supplied `x-kodehub-ui` header and sets it only for a human signed in with a session,
//! so agents (external or containerized) can never perform them.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use kodehub_store::identity::Permission;
use kodehub_store::Principal;
use serde_json::json;

use crate::AppState;

/// Routes reachable without a token.
const PUBLIC: &[&str] = &[
    "/api/health",
    "/api/auth/login",
    "/api/auth/register",
    "/api/auth/status",
];

/// Route prefixes that are not about one project (no project scope is applied).
const UNSCOPED: &[&str] = &[
    "/api/auth/",
    "/api/me",
    "/api/projects",
    "/api/orgs",
    "/api/agent-users",
    "/api/tokens",
    "/api/scopes/",
    "/api/credentials",
    "/api/audit",
    "/api/users",
];

/// The selected project for a request.
#[derive(Debug, Clone)]
pub struct ProjectCtx {
    pub id: String,
    pub permission: Permission,
}

fn query_param(q: Option<&str>, name: &str) -> Option<String> {
    q?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == name).then(|| v.to_string())
    })
}

fn unauthorized(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"error": msg, "kind": "unauthorized"})),
    )
        .into_response()
}

fn forbidden(msg: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(json!({"error": msg, "kind": "forbidden"})),
    )
        .into_response()
}

/// Pick the project for a request: explicit, else the caller's default, else the only project
/// an internal token is bound to, else the first visible project.
async fn choose_project(
    s: &AppState,
    p: &Principal,
    explicit: Option<String>,
) -> Result<Option<String>, Box<Response>> {
    if let Some(x) = explicit.filter(|x| !x.is_empty()) {
        return Ok(Some(x));
    }
    if let Some(ps) = p.scope.as_ref().and_then(|sc| sc.projects.as_ref()) {
        if ps.len() == 1 {
            return Ok(Some(ps[0].clone()));
        }
    }
    let user = kodehub_store::settings::human_of(p);
    if let Ok(u) = s.store.get_user(&user).await {
        if let Some(d) = u["default_project"].as_str() {
            if matches!(s.store.project_permission(p, d).await, Ok(Some(_))) {
                return Ok(Some(d.to_string()));
            }
        }
    }
    match s.store.visible_projects(p).await {
        Ok(v) => Ok(v.first().and_then(|x| x["id"].as_str()).map(str::to_string)),
        Err(e) => Err(Box::new(crate::ApiError::from(e).into_response())),
    }
}

/// The project an unscoped route is about, for the audit trail
/// (`/api/projects/{id}/…`, `/api/scopes/project:{id}/…`).
fn project_in_path(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix("/api/projects/") {
        let id = rest.split('/').next()?;
        return (!id.is_empty() && id != "contexts").then(|| id.to_string());
    }
    let rest = path.strip_prefix("/api/scopes/project:")?;
    Some(rest.split('/').next()?.to_string())
}

pub async fn auth_guard(State(s): State<AppState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    // the client may never assert "I am the UI" itself
    req.headers_mut().remove("x-kodehub-ui");
    if !path.starts_with("/api/") || PUBLIC.contains(&path.as_str()) {
        let method = req.method().clone();
        let resp = next.run(req).await;
        if method != Method::GET && path.starts_with("/api/auth/") {
            let _ = s
                .store
                .audit(
                    None,
                    None,
                    &format!("{method} {path}"),
                    resp.status().as_u16(),
                    json!({}),
                )
                .await;
        }
        return resp;
    }
    let token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string())
        .or_else(|| query_param(req.uri().query(), "token"));
    let Some(token) = token else {
        return unauthorized("sign in required");
    };
    let principal = match s.store.authenticate(&token).await {
        Ok(p) => p,
        Err(_) => return unauthorized("invalid or expired token"),
    };
    if principal.is_human() {
        if let Ok(v) = HeaderValue::from_str(&s.ui_token) {
            req.headers_mut().insert("x-kodehub-ui", v);
        }
    }
    let method = req.method().clone();
    let needs_project = !UNSCOPED.iter().any(|u| path.starts_with(u));
    let mut project: Option<ProjectCtx> = None;
    if needs_project {
        let explicit = req
            .headers()
            .get("x-kodehub-project")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| query_param(req.uri().query(), "project"));
        let id = match choose_project(&s, &principal, explicit).await {
            Ok(Some(id)) => id,
            Ok(None) => return forbidden("no project: create one or ask for access"),
            Err(r) => return *r,
        };
        let perm = match s.store.project_permission(&principal, &id).await {
            Ok(Some(p)) => p,
            Ok(None) => return forbidden(&format!("no access to project `{id}`")),
            Err(e) => return crate::ApiError::from(e).into_response(),
        };
        let need = if method == Method::GET {
            Permission::Read
        } else {
            Permission::Write
        };
        // reading endpoints that use POST for a query body
        let read_post = matches!(
            path.as_str(),
            "/api/query" | "/api/traverse" | "/api/releases/bisect"
        );
        if perm < need && !(read_post && perm >= Permission::Read) {
            return forbidden(&format!(
                "`{}` access to project `{id}` is required",
                need.as_str()
            ));
        }
        project = Some(ProjectCtx {
            id: id.clone(),
            permission: perm,
        });
    }
    req.extensions_mut().insert(principal.clone());
    if let Some(p) = &project {
        req.extensions_mut().insert(p.clone());
    }
    let resp = match &project {
        Some(p) => kodehub_store::with_project(p.id.clone(), next.run(req)).await,
        None => next.run(req).await,
    };
    if method != Method::GET {
        let _ = s
            .store
            .audit(
                Some(&principal),
                project
                    .as_ref()
                    .map(|p| p.id.clone())
                    .or_else(|| project_in_path(&path))
                    .as_deref(),
                &format!("{method} {path}"),
                resp.status().as_u16(),
                json!({}),
            )
            .await;
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_params() {
        assert_eq!(
            query_param(Some("a=1&token=x"), "token").as_deref(),
            Some("x")
        );
        assert_eq!(query_param(Some("tokens=x"), "token"), None);
        assert_eq!(query_param(None, "token"), None);
        assert_eq!(
            project_in_path("/api/projects/abc/shares").as_deref(),
            Some("abc")
        );
        assert_eq!(
            project_in_path("/api/scopes/project:abc/settings/x").as_deref(),
            Some("abc")
        );
        assert_eq!(project_in_path("/api/scopes/org:x/settings"), None);
    }
}
