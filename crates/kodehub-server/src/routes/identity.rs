//! Accounts, sessions, orgs, projects and sharing, agent users and tokens, layered settings,
//! credentials and the audit log (spec §6, §11, §12.1, §13A).

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::{Extension, Json};
use kodehub_store::audit::AuditFilter;
use kodehub_store::identity::{Owner, Permission};
use kodehub_store::Principal;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

fn forbid(m: impl Into<String>) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::FORBIDDEN,
        kind: "forbidden",
        message: m.into(),
    }
}

fn human(p: &Principal) -> Result<(), ApiError> {
    if p.is_human() {
        Ok(())
    } else {
        Err(forbid("this action needs a signed-in human"))
    }
}

// ---------------------------------------------------------------------- auth

#[derive(Deserialize)]
pub struct Register {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub display_name: String,
}

pub async fn status(State(s): State<AppState>) -> ApiResult<Value> {
    let users = s.store.user_count().await?;
    let open = s
        .store
        .resolve_chain(&["app".into()])
        .await?
        .get("open_registration")
        .map(|v| v["value"] == true)
        .unwrap_or(true);
    Ok(Json(
        json!({"has_users": users > 0, "open_registration": open || users == 0}),
    ))
}

pub async fn register(State(s): State<AppState>, Json(b): Json<Register>) -> ApiResult<Value> {
    let st = status(State(s.clone())).await?.0;
    if st["open_registration"] != true {
        return Err(forbid("registration is closed; ask an admin"));
    }
    let user = s
        .store
        .register_user(&b.username, &b.password, &b.display_name)
        .await?;
    let (token, _) = s.store.login(&b.username, &b.password).await?;
    Ok(Json(json!({"token": token, "user": user})))
}

#[derive(Deserialize)]
pub struct Login {
    pub username: String,
    pub password: String,
}

pub async fn login(State(s): State<AppState>, Json(b): Json<Login>) -> ApiResult<Value> {
    let (token, user) = s
        .store
        .login(&b.username, &b.password)
        .await
        .map_err(|_| ApiError {
            status: axum::http::StatusCode::UNAUTHORIZED,
            kind: "unauthorized",
            message: "wrong username or password".into(),
        })?;
    Ok(Json(json!({"token": token, "user": user})))
}

pub async fn logout(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Value> {
    if let Some(t) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        s.store.logout(t.trim()).await?;
    }
    Ok(Json(json!({"ok": true})))
}

pub async fn me(State(s): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Value> {
    let user = match p.via {
        kodehub_store::identity::AuthVia::Internal => Value::Null,
        _ => kodehub_store::identity::public_user(&s.store.get_user(&p.user).await?),
    };
    let orgs = if p.is_human() {
        s.store.orgs_of(&p.user).await?
    } else {
        vec![]
    };
    Ok(Json(json!({
        "principal": p, "human": p.is_human(), "user": user,
        "actor_class": kodehub_store::audit::actor_class(&p), "orgs": orgs,
    })))
}

pub async fn users(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Value> {
    human(&p)?;
    let rows = s
        .store
        .db
        .aql_raw(
            "FOR u IN users FILTER u.kind == 'human' AND u.disabled != true
               SORT u.username RETURN {key: u._key, display_name: u.display_name}",
            json!({}),
        )
        .await?;
    Ok(Json(json!(rows)))
}

// ---------------------------------------------------------------------- projects

pub async fn list_projects(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Value> {
    Ok(Json(json!(s.store.visible_projects(&p).await?)))
}

#[derive(Deserialize)]
pub struct NewProject {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Owner org (default: the caller).
    #[serde(default)]
    pub org: Option<String>,
    /// Users to share with: `{user: permission}`.
    #[serde(default)]
    pub share_users: std::collections::BTreeMap<String, String>,
    /// Share with the owner org's members (org-owned projects are shared with the org already).
    #[serde(default)]
    pub share_org: Option<String>,
    #[serde(default)]
    pub philosophy: Option<String>,
    /// A raw speech transcript of the philosophy (the architect cleans it up, spec §14.1).
    #[serde(default)]
    pub raw_transcript: Option<String>,
    /// `master` (the built-in template), `project:<id>` (copy another project's contexts and
    /// requirements) or none (start from scratch).
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub contexts: Vec<String>,
}

pub async fn create_project(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(b): Json<NewProject>,
) -> ApiResult<Value> {
    human(&p)?;
    let created = s
        .store
        .create_owned_project(&p, &b.name, &b.description, b.org.as_deref())
        .await?;
    let id = created["id"].as_str().unwrap_or_default().to_string();
    for (u, perm) in &b.share_users {
        s.store
            .share_project(
                &p,
                &id,
                &Owner::User(u.clone()),
                Some(Permission::parse(perm)?),
            )
            .await?;
    }
    if let Some(o) = &b.share_org {
        s.store
            .share_project(&p, &id, &Owner::Org(o.clone()), Some(Permission::Write))
            .await?;
    }
    let mut out = created;
    let mut contexts_in = b.contexts.clone();
    let master = b.template.as_deref() == Some("master");
    if master {
        for c in crate::routes::project_init::master_template()["contexts"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            if let Some(c) = c.as_str() {
                if !contexts_in.iter().any(|x| x == c) {
                    contexts_in.push(c.to_string());
                }
            }
        }
    }
    let init = crate::routes::project_init::initialize(
        &s,
        &p,
        &id,
        b.philosophy.as_deref(),
        b.raw_transcript.as_deref(),
        &contexts_in,
    )
    .await?;
    if master {
        let ps = s.store.for_project(&id);
        for r in crate::routes::project_init::master_template()["requirements"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let ctx = ps.db.pid(
                "contexts",
                &kodehub_store::slug(r["context"].as_str().unwrap_or("")),
            )?;
            let mut doc = r.clone();
            doc.as_object_mut().map(|m| m.remove("context"));
            let d = ps.create_requirement(doc).await?;
            let rid = d["_id"].as_str().unwrap_or_default().to_string();
            ps.insert_edge("applies_to", &rid, &ctx, json!({})).await?;
        }
        out["template"] = json!("master");
    } else if let Some(from) = b
        .template
        .as_deref()
        .and_then(|t| t.strip_prefix("project:"))
    {
        if s.store.project_permission(&p, from).await?.is_none() {
            return Err(forbid(format!("no access to template project `{from}`")));
        }
        out["template"] = crate::routes::project_init::copy_template(&s, from, &id).await?;
    }
    // the kickoff chain (spec §14.2): architect → planning agents → tests → code
    let contexts: Vec<String> = init["contexts"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if !contexts.is_empty()
        && s.store
            .setting(&id, "kickoff")
            .await?
            .as_bool()
            .unwrap_or(true)
    {
        let chain = crate::orchestrator::enqueue_kickoff(
            &s.store,
            &id,
            &contexts,
            b.raw_transcript.is_some(),
            &p.id(),
        )
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
        out["kickoff"] = json!(chain.iter().map(|w| w["_key"].clone()).collect::<Vec<_>>());
    }
    out["initialized"] = init;
    s.store.set_default_project(&p.user, &id).await?;
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct PatchProject {
    pub name: Option<String>,
    pub description: Option<String>,
}

async fn require_admin(s: &AppState, p: &Principal, id: &str) -> Result<(), ApiError> {
    if s.store.project_permission(p, id).await? != Some(Permission::Admin) {
        return Err(forbid(format!(
            "admin access to project `{id}` is required"
        )));
    }
    Ok(())
}

pub async fn patch_project(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(b): Json<PatchProject>,
) -> ApiResult<Value> {
    require_admin(&s, &p, &id).await?;
    Ok(Json(
        s.store
            .update_project(&id, b.name.as_deref(), b.description.as_deref())
            .await?,
    ))
}

pub async fn delete_project(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    human(&p)?;
    require_admin(&s, &p, &id).await?;
    if s.store.project_id().as_deref() == Some(id.as_str()) {
        // the server's fallback project: hand the fallback to another project first
        if let Some(other) = s
            .store
            .list_projects()
            .await?
            .iter()
            .filter_map(|x| x["id"].as_str())
            .find(|x| *x != id)
        {
            s.store.switch_project(other).await?;
        }
    }
    s.store.delete_project(&id).await?;
    Ok(Json(json!({"deleted": id})))
}

/// Make a project the caller's default (what the UI opens).
pub async fn open_project(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    if s.store.project_permission(&p, &id).await?.is_none() {
        return Err(forbid(format!("no access to project `{id}`")));
    }
    if p.is_human() {
        s.store.set_default_project(&p.user, &id).await?;
    }
    Ok(Json(json!({"project": id})))
}

pub async fn shares(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    if s.store.project_permission(&p, &id).await?.is_none() {
        return Err(forbid("no access"));
    }
    Ok(Json(json!({
        "owner": s.store.project_owner(&id).await?.map(|o| o.id()),
        "shares": s.store.project_shares(&id).await?,
    })))
}

#[derive(Deserialize)]
pub struct Share {
    /// `users/<u>` or `orgs/<o>` (or `user:<u>` / `org:<o>`).
    pub grantee: String,
    /// read | write | admin; null removes the share.
    pub permission: Option<String>,
}

fn parse_grantee(g: &str) -> Result<Owner, ApiError> {
    if let Some(o) = Owner::from_id(g) {
        return Ok(o);
    }
    match g.split_once(':') {
        Some(("user", u)) => Ok(Owner::User(u.into())),
        Some(("org", o)) => Ok(Owner::Org(o.into())),
        _ => Err(ApiError::bad_request(
            "grantee must be user:<name> or org:<name>",
        )),
    }
}

pub async fn put_share(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(b): Json<Share>,
) -> ApiResult<Value> {
    human(&p)?;
    let perm = b.permission.as_deref().map(Permission::parse).transpose()?;
    s.store
        .share_project(&p, &id, &parse_grantee(&b.grantee)?, perm)
        .await?;
    shares(State(s), Extension(p), Path(id)).await
}

// ---------------------------------------------------------------------- orgs

pub async fn list_orgs(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Value> {
    Ok(Json(json!(
        s.store
            .orgs_of(&kodehub_store::settings::human_of(&p))
            .await?
    )))
}

#[derive(Deserialize)]
pub struct NewOrg {
    pub name: String,
}

pub async fn create_org(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(b): Json<NewOrg>,
) -> ApiResult<Value> {
    human(&p)?;
    Ok(Json(s.store.create_org(&p, &b.name).await?))
}

pub async fn org_members(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(o): Path<String>,
) -> ApiResult<Value> {
    if s.store
        .org_role(&o, &kodehub_store::settings::human_of(&p))
        .await?
        .is_none()
    {
        return Err(forbid("not a member"));
    }
    Ok(Json(
        json!({"org": s.store.get_org(&o).await?, "members": s.store.org_members(&o).await?}),
    ))
}

#[derive(Deserialize)]
pub struct Member {
    pub role: String,
}

pub async fn put_member(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path((o, u)): Path<(String, String)>,
    Json(b): Json<Member>,
) -> ApiResult<Value> {
    human(&p)?;
    s.store.set_org_member(&p, &o, &u, &b.role).await?;
    org_members(State(s), Extension(p), Path(o)).await
}

pub async fn delete_member(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path((o, u)): Path<(String, String)>,
) -> ApiResult<Value> {
    human(&p)?;
    s.store.remove_org_member(&p, &o, &u).await?;
    Ok(Json(json!({"org": s.store.get_org(&o).await?})))
}

// ---------------------------------------------------------------------- agent users & tokens

pub async fn list_agent_users(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Value> {
    human(&p)?;
    Ok(Json(json!(s.store.list_agent_users(&p).await?)))
}

#[derive(Deserialize)]
pub struct NewAgent {
    pub name: String,
    #[serde(default)]
    pub org: Option<String>,
}

pub async fn create_agent_user(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(b): Json<NewAgent>,
) -> ApiResult<Value> {
    human(&p)?;
    Ok(Json(
        s.store
            .create_agent_user(&p, &b.name, b.org.as_deref())
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct Disable {
    #[serde(default = "yes")]
    pub disabled: bool,
}

fn yes() -> bool {
    true
}

pub async fn disable_agent_user(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(key): Path<String>,
    Json(b): Json<Disable>,
) -> ApiResult<Value> {
    human(&p)?;
    s.store.set_user_disabled(&p, &key, b.disabled).await?;
    Ok(Json(json!({"user": key, "disabled": b.disabled})))
}

pub async fn list_tokens(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Value> {
    human(&p)?;
    Ok(Json(json!(s.store.list_tokens(&p).await?)))
}

#[derive(Deserialize)]
pub struct NewToken {
    pub agent: String,
    pub name: String,
    #[serde(default)]
    pub projects: Option<Vec<String>>,
    #[serde(default = "write")]
    pub permission: String,
    #[serde(default)]
    pub expires_in_days: Option<i64>,
}

fn write() -> String {
    "write".into()
}

pub async fn create_token(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(b): Json<NewToken>,
) -> ApiResult<Value> {
    human(&p)?;
    let (token, rec) = s
        .store
        .issue_token(
            &p,
            &b.agent,
            &b.name,
            b.projects,
            Permission::parse(&b.permission)?,
            b.expires_in_days,
        )
        .await?;
    Ok(Json(json!({"token": token, "record": rec,
                   "note": "This token is shown once. Store it in your assistant's MCP settings."})))
}

pub async fn revoke_token(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    human(&p)?;
    s.store.revoke_token(&p, &id).await?;
    Ok(Json(json!({"revoked": id})))
}

// ---------------------------------------------------------------------- layered settings

pub async fn scope_settings(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(scope): Path<String>,
) -> ApiResult<Value> {
    if !s.store.can_view_scope(&p, &scope).await? {
        return Err(forbid(format!("you may not view settings at {scope}")));
    }
    let stored = s.store.scope_settings(&scope).await?;
    let resolved = match kodehub_store::settings::parse_scope(&scope)? {
        ("project", id) => s.store.resolve_settings(id, Some(&p)).await?,
        ("app", _) => s.store.resolve_chain(&["app".into()]).await?,
        _ => {
            s.store
                .resolve_chain(&["app".into(), scope.clone()])
                .await?
        }
    };
    Ok(Json(json!({
        "scope": scope, "stored": stored, "resolved": resolved,
        "can_edit": s.store.can_manage_scope(&p, &scope).await?,
    })))
}

#[derive(Deserialize)]
pub struct SetValue {
    pub value: Value,
}

pub async fn put_setting(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path((scope, key)): Path<(String, String)>,
    Json(b): Json<SetValue>,
) -> ApiResult<Value> {
    s.store.set_setting(&p, &scope, &key, Some(b.value)).await?;
    scope_settings(State(s), Extension(p), Path(scope)).await
}

pub async fn delete_setting(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path((scope, key)): Path<(String, String)>,
) -> ApiResult<Value> {
    s.store.set_setting(&p, &scope, &key, None).await?;
    scope_settings(State(s), Extension(p), Path(scope)).await
}

// ---------------------------------------------------------------------- credentials

#[derive(Deserialize)]
pub struct ScopeQ {
    pub scope: String,
}

pub async fn list_credentials(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Query(q): Query<ScopeQ>,
) -> ApiResult<Value> {
    Ok(Json(json!(s.store.list_credentials(&p, &q.scope).await?)))
}

#[derive(Deserialize)]
pub struct NewCredential {
    pub scope: String,
    #[serde(default = "anthropic")]
    pub provider: String,
    pub kind: String,
    pub secret: String,
    #[serde(default)]
    pub label: String,
}

fn anthropic() -> String {
    "anthropic".into()
}

pub async fn put_credential(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(b): Json<NewCredential>,
) -> ApiResult<Value> {
    human(&p)?;
    Ok(Json(
        s.store
            .put_credential(&p, &b.scope, &b.provider, &b.kind, &b.secret, &b.label)
            .await?,
    ))
}

pub async fn delete_credential(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Value> {
    human(&p)?;
    s.store.delete_credential(&p, &id).await?;
    Ok(Json(json!({"deleted": id})))
}

// ---------------------------------------------------------------------- audit

#[derive(Deserialize, Default)]
pub struct AuditQ {
    pub project: Option<String>,
    pub user: Option<String>,
    /// human | external_agent | containerized_agent
    pub class: Option<String>,
    #[serde(default)]
    pub limit: usize,
}

pub async fn audit(
    State(s): State<AppState>,
    Extension(p): Extension<Principal>,
    Query(q): Query<AuditQ>,
) -> ApiResult<Value> {
    human(&p)?;
    let mut f = AuditFilter {
        project: q.project.clone(),
        user: q.user.clone(),
        actor_class: q.class.clone(),
        limit: q.limit,
    };
    if !p.is_admin {
        match &q.project {
            Some(id) if s.store.project_permission(&p, id).await? == Some(Permission::Admin) => {}
            // non-admins see their own actions and their agents'
            _ => f.user = Some(p.id()),
        }
    }
    Ok(Json(json!(s.store.list_audit(&f).await?)))
}
