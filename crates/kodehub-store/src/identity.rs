//! Users (human and agent), organizations, sessions, personal access tokens, project ownership
//! and permissions (spec §6, §11.1, §13A).
//!
//! These records are global (not inside a project's collection set). Ownership and permissions
//! are edges: `kodehub_member_of` (user → org, `role`), `kodehub_access` (user | org → project,
//! `permission`) and `kodehub_acts_for` (agent user → human user | org). A project's owner is
//! recorded on its registry document (`owner`).
//!
//! Secrets are never stored: passwords as argon2 hashes, session and access tokens as BLAKE3
//! hashes of high-entropy random tokens (shown once at creation).

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use kodehub_core::model::now_iso;
use rand::RngCore;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::schema::PROJECTS;
use crate::{Result, Store, StoreError};

pub const USERS: &str = "kodehub_users";
pub const ORGS: &str = "kodehub_orgs";
pub const MEMBER_OF: &str = "kodehub_member_of";
pub const ACCESS: &str = "kodehub_access";
pub const ACTS_FOR: &str = "kodehub_acts_for";
pub const SESSIONS: &str = "kodehub_sessions";
pub const TOKENS: &str = "kodehub_tokens";

/// Global document collections owned by this module.
pub const IDENTITY_DOCS: &[&str] = &[USERS, ORGS, SESSIONS, TOKENS];
/// Global edge collections owned by this module.
pub const IDENTITY_EDGES: &[&str] = &[MEMBER_OF, ACCESS, ACTS_FOR];

const SESSION_PREFIX: &str = "khs_";
const PAT_PREFIX: &str = "khp_";
const INTERNAL_PREFIX: &str = "khi_";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    Write,
    Admin,
}

impl Permission {
    pub fn parse(s: &str) -> Result<Permission> {
        match s {
            "read" => Ok(Permission::Read),
            "write" => Ok(Permission::Write),
            "admin" => Ok(Permission::Admin),
            other => Err(StoreError::Invalid(format!(
                "permission must be read, write or admin (got `{other}`)"
            ))),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::Read => "read",
            Permission::Write => "write",
            Permission::Admin => "admin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserKind {
    Human,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthVia {
    Session,
    Pat,
    /// Short-lived token issued by the orchestrator to a containerized agent (spec §13A.2).
    Internal,
}

/// Limits carried by a token: which projects and at most which permission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenScope {
    /// `None` = every project the identity behind the token can reach.
    pub projects: Option<Vec<String>>,
    pub permission: Permission,
    /// Internal tokens are bound to one work item.
    #[serde(default)]
    pub work_item: Option<String>,
}

/// The authenticated caller of an API request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    /// User key (`kodehub_users/<key>`). For internal tokens, the agent role name.
    pub user: String,
    pub kind: UserKind,
    /// The human user (`kodehub_users/x`) or org (`kodehub_orgs/y`) an agent acts for.
    pub acts_for: Option<String>,
    pub via: AuthVia,
    pub scope: Option<TokenScope>,
    pub is_admin: bool,
    pub display_name: String,
    /// Token record key (for audit and revocation).
    pub token_id: Option<String>,
}

impl Principal {
    pub fn id(&self) -> String {
        format!("{USERS}/{}", self.user)
    }
    /// A human signed in with a session (the only identity allowed human-only actions).
    pub fn is_human(&self) -> bool {
        self.kind == UserKind::Human && self.via == AuthVia::Session
    }
    /// The human this identity ultimately acts for (itself for a human).
    pub fn on_behalf_of(&self) -> Option<String> {
        match self.kind {
            UserKind::Human => None,
            UserKind::Agent => self.acts_for.clone(),
        }
    }
}

/// Owner of a project.
#[derive(Debug, Clone, PartialEq)]
pub enum Owner {
    User(String),
    Org(String),
}

impl Owner {
    pub fn id(&self) -> String {
        match self {
            Owner::User(u) => format!("{USERS}/{u}"),
            Owner::Org(o) => format!("{ORGS}/{o}"),
        }
    }
    pub fn from_id(id: &str) -> Option<Owner> {
        let (c, k) = id.split_once('/')?;
        match c {
            USERS => Some(Owner::User(k.to_string())),
            ORGS => Some(Owner::Org(k.to_string())),
            _ => None,
        }
    }
}

pub fn random_token(prefix: &str) -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{prefix}{hex}")
}

/// Storage key of a token: a hash, so the database never holds a usable token.
pub fn token_key(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex()[..40].to_string()
}

pub fn valid_username(u: &str) -> bool {
    (2..=40).contains(&u.len())
        && u.starts_with(|c: char| c.is_ascii_lowercase())
        && u.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.".contains(c))
}

fn hash_password(pw: &str) -> Result<String> {
    if pw.chars().count() < 8 {
        return Err(StoreError::Invalid(
            "password must be at least 8 characters".into(),
        ));
    }
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| StoreError::Invalid(format!("password hashing failed: {e}")))
}

fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

fn add_days(days: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::days(days))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn add_secs(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn expired(at: &Value) -> bool {
    at.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|t| t < chrono::Utc::now())
}

/// Public view of a user document (no password hash).
pub fn public_user(u: &Value) -> Value {
    json!({
        "id": u["_id"], "key": u["_key"], "username": u["username"], "kind": u["kind"],
        "display_name": u["display_name"], "is_admin": u["is_admin"] == true,
        "disabled": u["disabled"] == true, "created_at": u["created_at"],
        "default_project": u["default_project"],
    })
}

impl Store {
    // ------------------------------------------------------------------ global doc helpers

    pub(crate) async fn g_insert(&self, coll: &str, doc: &Value) -> Result<Value> {
        self.db
            .db_raw(
                Method::POST,
                &format!("/_api/document/{coll}?returnNew=true"),
                Some(doc),
            )
            .await
            .map(|v| v["new"].clone())
    }

    pub(crate) async fn g_get(&self, coll: &str, key: &str) -> Result<Option<Value>> {
        if !crate::arango::valid_key(key) {
            return Ok(None);
        }
        match self
            .db
            .db_raw(Method::GET, &format!("/_api/document/{coll}/{key}"), None)
            .await
        {
            Ok(v) => Ok(Some(v)),
            Err(StoreError::Arango { num: 1202, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn g_patch(&self, coll: &str, key: &str, patch: &Value) -> Result<Value> {
        self.db
            .db_raw(
                Method::PATCH,
                &format!("/_api/document/{coll}/{key}?returnNew=true"),
                Some(patch),
            )
            .await
            .map(|v| v["new"].clone())
    }

    pub(crate) async fn g_aql(&self, q: &str, bind: Value) -> Result<Vec<Value>> {
        self.db.aql_raw(q, bind).await
    }

    // ------------------------------------------------------------------ users

    pub async fn user_count(&self) -> Result<u64> {
        Ok(self
            .g_aql(
                "RETURN LENGTH(FOR u IN @@u FILTER u.kind == 'human' RETURN 1)",
                json!({"@u": USERS}),
            )
            .await?
            .first()
            .and_then(Value::as_u64)
            .unwrap_or(0))
    }

    /// Register a human user. The first human becomes the system admin and inherits any
    /// project that has no owner yet.
    pub async fn register_user(
        &self,
        username: &str,
        password: &str,
        display_name: &str,
    ) -> Result<Value> {
        let username = username.trim().to_ascii_lowercase();
        if !valid_username(&username) {
            return Err(StoreError::Invalid(
                "username: 2–40 characters, lower-case letters, digits, '-', '_' or '.', \
                 starting with a letter"
                    .into(),
            ));
        }
        let first = self.user_count().await? == 0;
        let doc = json!({
            "_key": username, "username": username, "kind": "human",
            "display_name": if display_name.trim().is_empty() { username.clone() } else { display_name.trim().to_string() },
            "password_hash": hash_password(password)?,
            "is_admin": first, "disabled": false, "created_at": now_iso(),
        });
        let created = match self.g_insert(USERS, &doc).await {
            Err(e) if e.is_unique_violation() => {
                return Err(StoreError::Conflict(format!(
                    "username `{username}` is taken"
                )))
            }
            r => r?,
        };
        if first {
            self.g_aql(
                "FOR p IN @@p FILTER p.owner == null UPDATE p WITH {owner: @o} IN @@p",
                json!({"@p": PROJECTS, "o": format!("{USERS}/{username}")}),
            )
            .await?;
        }
        Ok(public_user(&created))
    }

    pub async fn get_user(&self, key: &str) -> Result<Value> {
        self.g_get(USERS, key)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("user {key}")))
    }

    /// Sign in a human: returns `(session token, user)`. The token is shown only here.
    pub async fn login(&self, username: &str, password: &str) -> Result<(String, Value)> {
        let username = username.trim().to_ascii_lowercase();
        let bad = || StoreError::Invalid("wrong username or password".into());
        let u = self.g_get(USERS, &username).await?.ok_or_else(bad)?;
        if u["kind"] != "human" || u["disabled"] == true {
            return Err(bad());
        }
        if !verify_password(password, u["password_hash"].as_str().unwrap_or("")) {
            return Err(bad());
        }
        let token = random_token(SESSION_PREFIX);
        let ttl_hours = self
            .setting_for_user(&username, "session_ttl_hours")
            .await?
            .as_i64()
            .unwrap_or(168);
        self.g_insert(
            SESSIONS,
            &json!({
                "_key": token_key(&token), "user": username,
                "created_at": now_iso(), "expires_at": add_secs(ttl_hours * 3600),
            }),
        )
        .await?;
        Ok((token, public_user(&u)))
    }

    pub async fn logout(&self, token: &str) -> Result<()> {
        let key = token_key(token);
        match self
            .db
            .db_raw(
                Method::DELETE,
                &format!("/_api/document/{SESSIONS}/{key}"),
                None,
            )
            .await
        {
            Ok(_) | Err(StoreError::Arango { num: 1202, .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Disable (or re-enable) a user. Takes effect on the next call: sessions and tokens of a
    /// disabled user, and of agents acting for them, stop authenticating.
    pub async fn set_user_disabled(
        &self,
        by: &Principal,
        user: &str,
        disabled: bool,
    ) -> Result<()> {
        let target = self.get_user(user).await?;
        let allowed = by.is_admin && by.is_human()
            || (target["kind"] == "agent" && self.controls_agent(by, user).await?);
        if !allowed {
            return Err(StoreError::Invalid("not allowed".into()));
        }
        self.g_patch(USERS, user, &json!({"disabled": disabled}))
            .await?;
        Ok(())
    }

    pub async fn set_default_project(&self, user: &str, project: &str) -> Result<()> {
        self.g_patch(USERS, user, &json!({"default_project": project}))
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------------ agent users & tokens

    /// Create an external agent user (spec §13A.1) acting for the calling human, or for an org
    /// the caller administers.
    pub async fn create_agent_user(
        &self,
        by: &Principal,
        name: &str,
        for_org: Option<&str>,
    ) -> Result<Value> {
        if !by.is_human() {
            return Err(StoreError::Invalid(
                "only a signed-in human can create agent users".into(),
            ));
        }
        let acts_for = match for_org {
            Some(o) => {
                if self.org_role(o, &by.user).await? != Some("admin".into()) {
                    return Err(StoreError::Invalid(format!(
                        "you are not an admin of org {o}"
                    )));
                }
                format!("{ORGS}/{o}")
            }
            None => by.id(),
        };
        let slug = crate::slug(name);
        if slug.is_empty() {
            return Err(StoreError::Invalid("agent name is required".into()));
        }
        let key = format!("agent-{}-{slug}", by.user);
        let key: String = key.chars().take(60).collect();
        let doc = json!({
            "_key": key, "username": key, "kind": "agent", "display_name": name.trim(),
            "is_admin": false, "disabled": false, "created_at": now_iso(), "created_by": by.id(),
        });
        let created = match self.g_insert(USERS, &doc).await {
            Err(e) if e.is_unique_violation() => {
                return Err(StoreError::Conflict(format!(
                    "agent `{name}` already exists"
                )))
            }
            r => r?,
        };
        self.g_insert(
            ACTS_FOR,
            &json!({"_from": format!("{USERS}/{key}"), "_to": acts_for, "created_at": now_iso()}),
        )
        .await?;
        let mut out = public_user(&created);
        out["acts_for"] = json!(acts_for);
        Ok(out)
    }

    async fn acts_for_of(&self, agent: &str) -> Result<Option<String>> {
        Ok(self
            .g_aql(
                "FOR e IN @@a FILTER e._from == @u LIMIT 1 RETURN e._to",
                json!({"@a": ACTS_FOR, "u": format!("{USERS}/{agent}")}),
            )
            .await?
            .first()
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// True when `by` (a human) may manage agent user `agent`.
    async fn controls_agent(&self, by: &Principal, agent: &str) -> Result<bool> {
        if !by.is_human() {
            return Ok(false);
        }
        Ok(match self.acts_for_of(agent).await? {
            Some(id) if id == by.id() => true,
            Some(id) => match Owner::from_id(&id) {
                Some(Owner::Org(o)) => self.org_role(&o, &by.user).await? == Some("admin".into()),
                _ => false,
            },
            None => false,
        })
    }

    /// Agent users the caller manages.
    pub async fn list_agent_users(&self, by: &Principal) -> Result<Vec<Value>> {
        let rows = self
            .g_aql(
                "FOR u IN @@u FILTER u.kind == 'agent'
                   LET af = FIRST(FOR e IN @@a FILTER e._from == u._id RETURN e._to)
                   SORT u.created_at RETURN MERGE(u, {acts_for: af})",
                json!({"@u": USERS, "@a": ACTS_FOR}),
            )
            .await?;
        let mut out = Vec::new();
        for r in rows {
            let key = r["_key"].as_str().unwrap_or_default();
            if self.controls_agent(by, key).await? {
                let mut p = public_user(&r);
                p["acts_for"] = r["acts_for"].clone();
                out.push(p);
            }
        }
        Ok(out)
    }

    /// Issue a personal access token to an agent user the caller manages. Returns the token
    /// (shown once) and its record. Expiry is capped by the `token_max_lifetime_days` setting.
    pub async fn issue_token(
        &self,
        by: &Principal,
        agent: &str,
        name: &str,
        projects: Option<Vec<String>>,
        permission: Permission,
        expires_in_days: Option<i64>,
    ) -> Result<(String, Value)> {
        if !self.controls_agent(by, agent).await? {
            return Err(StoreError::Invalid(format!(
                "you do not manage agent user `{agent}`"
            )));
        }
        let max = self
            .setting_for_user(&by.user, "token_max_lifetime_days")
            .await?
            .as_i64()
            .unwrap_or(90);
        let days = expires_in_days.unwrap_or(max).clamp(1, max);
        let token = random_token(PAT_PREFIX);
        let rec = json!({
            "_key": token_key(&token), "user": agent, "name": name.trim(),
            "scope": {"projects": projects, "permission": permission},
            "created_at": now_iso(), "created_by": by.id(), "expires_at": add_days(days),
            "revoked": false, "prefix": &token[..12],
        });
        let created = self.g_insert(TOKENS, &rec).await?;
        Ok((token, public_token(&created)))
    }

    pub async fn list_tokens(&self, by: &Principal) -> Result<Vec<Value>> {
        let rows = self
            .g_aql(
                "FOR t IN @@t FILTER t.revoked != true AND t.kind != 'internal'
                   SORT t.created_at DESC RETURN t",
                json!({"@t": TOKENS}),
            )
            .await?;
        let mut out = Vec::new();
        for t in rows {
            if self
                .controls_agent(by, t["user"].as_str().unwrap_or_default())
                .await?
            {
                out.push(public_token(&t));
            }
        }
        Ok(out)
    }

    pub async fn revoke_token(&self, by: &Principal, id: &str) -> Result<()> {
        let t = self
            .g_get(TOKENS, id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("token {id}")))?;
        if !self
            .controls_agent(by, t["user"].as_str().unwrap_or_default())
            .await?
        {
            return Err(StoreError::Invalid("not allowed".into()));
        }
        self.g_patch(
            TOKENS,
            id,
            &json!({"revoked": true, "revoked_at": now_iso()}),
        )
        .await?;
        Ok(())
    }

    /// Issue a short-lived internal token for a containerized agent working one work item in one
    /// project (spec §13A.2). It expires with the work item's lease.
    pub async fn issue_internal_token(
        &self,
        role: &str,
        project: &str,
        work_item: &str,
        permission: Permission,
        ttl_secs: i64,
    ) -> Result<String> {
        let token = random_token(INTERNAL_PREFIX);
        self.g_insert(
            TOKENS,
            &json!({
                "_key": token_key(&token), "kind": "internal", "user": role,
                "scope": {"projects": [project], "permission": permission, "work_item": work_item},
                "created_at": now_iso(), "expires_at": add_secs(ttl_secs), "revoked": false,
            }),
        )
        .await?;
        Ok(token)
    }

    /// Revoke every internal token bound to a work item (called when its lease ends).
    pub async fn revoke_work_item_tokens(&self, work_item: &str) -> Result<()> {
        self.g_aql(
            "FOR t IN @@t FILTER t.kind == 'internal' AND t.scope.work_item == @w
               UPDATE t WITH {revoked: true, revoked_at: @now} IN @@t",
            json!({"@t": TOKENS, "w": work_item, "now": now_iso()}),
        )
        .await?;
        Ok(())
    }

    /// Resolve a bearer token to a principal. Expired, revoked or disabled identities fail.
    pub async fn authenticate(&self, token: &str) -> Result<Principal> {
        let unauth = || StoreError::Invalid("invalid or expired token".into());
        let key = token_key(token);
        if token.starts_with(SESSION_PREFIX) {
            let s = self.g_get(SESSIONS, &key).await?.ok_or_else(unauth)?;
            if expired(&s["expires_at"]) {
                return Err(unauth());
            }
            let u = self
                .g_get(USERS, s["user"].as_str().unwrap_or_default())
                .await?
                .ok_or_else(unauth)?;
            if u["disabled"] == true || u["kind"] != "human" {
                return Err(unauth());
            }
            return Ok(Principal {
                user: u["_key"].as_str().unwrap_or_default().into(),
                kind: UserKind::Human,
                acts_for: None,
                via: AuthVia::Session,
                scope: None,
                is_admin: u["is_admin"] == true,
                display_name: u["display_name"].as_str().unwrap_or_default().into(),
                token_id: None,
            });
        }
        if !(token.starts_with(PAT_PREFIX) || token.starts_with(INTERNAL_PREFIX)) {
            return Err(unauth());
        }
        let t = self.g_get(TOKENS, &key).await?.ok_or_else(unauth)?;
        if t["revoked"] == true || expired(&t["expires_at"]) {
            return Err(unauth());
        }
        let scope: TokenScope = serde_json::from_value(t["scope"].clone()).map_err(|_| unauth())?;
        if t["kind"] == "internal" {
            let role = t["user"].as_str().unwrap_or_default().to_string();
            return Ok(Principal {
                display_name: format!("{role} agent"),
                user: role,
                kind: UserKind::Agent,
                acts_for: None,
                via: AuthVia::Internal,
                scope: Some(scope),
                is_admin: false,
                token_id: Some(key),
            });
        }
        let agent = t["user"].as_str().unwrap_or_default();
        let u = self.g_get(USERS, agent).await?.ok_or_else(unauth)?;
        if u["disabled"] == true {
            return Err(unauth());
        }
        let acts_for = self.acts_for_of(agent).await?;
        // the human behind the agent must still be active
        if let Some(Owner::User(h)) = acts_for.as_deref().and_then(Owner::from_id) {
            let hu = self.g_get(USERS, &h).await?.ok_or_else(unauth)?;
            if hu["disabled"] == true {
                return Err(unauth());
            }
        }
        self.g_patch(TOKENS, &key, &json!({"last_used_at": now_iso()}))
            .await?;
        Ok(Principal {
            user: agent.to_string(),
            kind: UserKind::Agent,
            acts_for,
            via: AuthVia::Pat,
            scope: Some(scope),
            is_admin: false,
            display_name: u["display_name"].as_str().unwrap_or_default().into(),
            token_id: Some(key),
        })
    }

    // ------------------------------------------------------------------ organizations

    pub async fn create_org(&self, by: &Principal, name: &str) -> Result<Value> {
        if !by.is_human() {
            return Err(StoreError::Invalid("only a human can create an org".into()));
        }
        let key = crate::slug(name);
        if key.is_empty() {
            return Err(StoreError::Invalid("org name is required".into()));
        }
        let created = match self
            .g_insert(
                ORGS,
                &json!({"_key": key, "name": name.trim(), "archived": false,
                         "created_at": now_iso(), "created_by": by.id()}),
            )
            .await
        {
            Err(e) if e.is_unique_violation() => {
                return Err(StoreError::Conflict(format!("org `{key}` exists")))
            }
            r => r?,
        };
        self.g_insert(
            MEMBER_OF,
            &json!({"_key": format!("{}-{key}", by.user), "_from": by.id(),
                     "_to": format!("{ORGS}/{key}"), "role": "admin", "created_at": now_iso()}),
        )
        .await?;
        Ok(created)
    }

    pub async fn get_org(&self, key: &str) -> Result<Value> {
        self.g_get(ORGS, key)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("org {key}")))
    }

    /// The user's role in an org (`admin` | `member`), if a member.
    pub async fn org_role(&self, org: &str, user: &str) -> Result<Option<String>> {
        Ok(self
            .g_aql(
                "FOR e IN @@m FILTER e._from == @u AND e._to == @o LIMIT 1 RETURN e.role",
                json!({"@m": MEMBER_OF, "u": format!("{USERS}/{user}"), "o": format!("{ORGS}/{org}")}),
            )
            .await?
            .first()
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    pub async fn orgs_of(&self, user: &str) -> Result<Vec<Value>> {
        self.g_aql(
            "FOR e IN @@m FILTER e._from == @u LET o = DOCUMENT(e._to)
               SORT o.name RETURN MERGE(o, {role: e.role})",
            json!({"@m": MEMBER_OF, "u": format!("{USERS}/{user}")}),
        )
        .await
    }

    pub async fn org_members(&self, org: &str) -> Result<Vec<Value>> {
        self.g_aql(
            "FOR e IN @@m FILTER e._to == @o LET u = DOCUMENT(e._from)
               SORT u.username RETURN {user: u._key, display_name: u.display_name, role: e.role}",
            json!({"@m": MEMBER_OF, "o": format!("{ORGS}/{org}")}),
        )
        .await
    }

    async fn require_org_admin(&self, by: &Principal, org: &str) -> Result<()> {
        let o = self.get_org(org).await?;
        if o["archived"] == true {
            return Err(StoreError::Conflict(format!(
                "org `{org}` is archived (it has no admins left)"
            )));
        }
        if !by.is_human() || self.org_role(org, &by.user).await? != Some("admin".into()) {
            return Err(StoreError::Invalid(format!(
                "you are not an admin of org {org}"
            )));
        }
        Ok(())
    }

    /// Add a user to an org, or change their role. Only org admins may do this.
    pub async fn set_org_member(
        &self,
        by: &Principal,
        org: &str,
        user: &str,
        role: &str,
    ) -> Result<()> {
        if !matches!(role, "admin" | "member") {
            return Err(StoreError::Invalid("role must be admin or member".into()));
        }
        self.require_org_admin(by, org).await?;
        let u = self.get_user(user).await?;
        if u["kind"] != "human" {
            return Err(StoreError::Invalid("only human users join orgs".into()));
        }
        let key = format!("{user}-{org}");
        self.db
            .db_raw(
                Method::POST,
                &format!("/_api/document/{MEMBER_OF}?overwriteMode=update"),
                Some(&json!({"_key": key, "_from": format!("{USERS}/{user}"),
                             "_to": format!("{ORGS}/{org}"), "role": role, "created_at": now_iso()})),
            )
            .await?;
        self.archive_org_if_adminless(org).await
    }

    /// Remove a user from an org (admins may remove anyone; members may leave).
    pub async fn remove_org_member(&self, by: &Principal, org: &str, user: &str) -> Result<()> {
        if by.user != user {
            self.require_org_admin(by, org).await?;
        }
        let key = format!("{user}-{org}");
        match self
            .db
            .db_raw(
                Method::DELETE,
                &format!("/_api/document/{MEMBER_OF}/{key}"),
                None,
            )
            .await
        {
            Ok(_) | Err(StoreError::Arango { num: 1202, .. }) => {}
            Err(e) => return Err(e),
        }
        self.archive_org_if_adminless(org).await
    }

    /// Spec §11.1: an org with zero admins is archived automatically.
    async fn archive_org_if_adminless(&self, org: &str) -> Result<()> {
        let admins = self
            .g_aql(
                "RETURN LENGTH(FOR e IN @@m FILTER e._to == @o AND e.role == 'admin' RETURN 1)",
                json!({"@m": MEMBER_OF, "o": format!("{ORGS}/{org}")}),
            )
            .await?
            .first()
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if admins == 0 {
            self.g_patch(
                ORGS,
                org,
                &json!({"archived": true, "archived_at": now_iso()}),
            )
            .await?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ project ownership

    pub async fn project_owner(&self, project: &str) -> Result<Option<Owner>> {
        Ok(self
            .g_get(PROJECTS, project)
            .await?
            .and_then(|p| p["owner"].as_str().and_then(Owner::from_id)))
    }

    pub async fn set_project_owner(&self, project: &str, owner: &Owner) -> Result<()> {
        self.g_patch(PROJECTS, project, &json!({"owner": owner.id()}))
            .await?;
        Ok(())
    }

    /// Share a project with a user or org at a permission level (project admins only).
    pub async fn share_project(
        &self,
        by: &Principal,
        project: &str,
        grantee: &Owner,
        permission: Option<Permission>,
    ) -> Result<()> {
        if self.project_permission(by, project).await? != Some(Permission::Admin) {
            return Err(StoreError::Invalid("only project admins can share".into()));
        }
        match grantee {
            Owner::User(u) => {
                self.get_user(u).await?;
            }
            Owner::Org(o) => {
                self.get_org(o).await?;
            }
        }
        let gid = grantee.id();
        let key = crate::store::edge_key(ACCESS, &gid, project);
        match permission {
            Some(p) => {
                self.db
                    .db_raw(
                        Method::POST,
                        &format!("/_api/document/{ACCESS}?overwriteMode=update"),
                        Some(&json!({"_key": key, "_from": gid, "_to": format!("{PROJECTS}/{project}"),
                                     "permission": p, "granted_by": by.id(), "granted_at": now_iso()})),
                    )
                    .await?;
            }
            None => match self
                .db
                .db_raw(
                    Method::DELETE,
                    &format!("/_api/document/{ACCESS}/{key}"),
                    None,
                )
                .await
            {
                Ok(_) | Err(StoreError::Arango { num: 1202, .. }) => {}
                Err(e) => return Err(e),
            },
        }
        Ok(())
    }

    pub async fn project_shares(&self, project: &str) -> Result<Vec<Value>> {
        self.g_aql(
            "FOR e IN @@a FILTER e._to == @p RETURN {grantee: e._from, permission: e.permission}",
            json!({"@a": ACCESS, "p": format!("{PROJECTS}/{project}")}),
        )
        .await
    }

    /// A human user's permission on a project (ownership, org membership, shares).
    async fn human_permission(&self, user: &str, project: &str) -> Result<Option<Permission>> {
        let Some(p) = self.g_get(PROJECTS, project).await? else {
            return Ok(None);
        };
        let uid = format!("{USERS}/{user}");
        let mut best: Option<Permission> = None;
        let mut bump = |p: Permission| {
            if best.is_none_or(|b| p > b) {
                best = Some(p);
            }
        };
        let orgs = self.orgs_of(user).await?;
        match p["owner"].as_str().and_then(Owner::from_id) {
            Some(Owner::User(u)) if u == user => bump(Permission::Admin),
            Some(Owner::Org(o)) => {
                if let Some(m) = orgs.iter().find(|x| x["_key"] == o.as_str()) {
                    if m["archived"] != true {
                        bump(if m["role"] == "admin" {
                            Permission::Admin
                        } else {
                            Permission::Write
                        });
                    }
                }
            }
            _ => {}
        }
        let mut grantees = vec![uid];
        grantees.extend(
            orgs.iter()
                .filter(|o| o["archived"] != true)
                .filter_map(|o| o["_id"].as_str().map(str::to_string)),
        );
        for row in self
            .g_aql(
                "FOR e IN @@a FILTER e._to == @p AND e._from IN @g RETURN e.permission",
                json!({"@a": ACCESS, "p": format!("{PROJECTS}/{project}"), "g": grantees}),
            )
            .await?
        {
            if let Some(s) = row.as_str() {
                bump(Permission::parse(s)?);
            }
        }
        Ok(best)
    }

    /// The caller's effective permission on a project, or `None` (no access). An agent never
    /// exceeds the human or org it acts for, nor its token's scope.
    pub async fn project_permission(
        &self,
        by: &Principal,
        project: &str,
    ) -> Result<Option<Permission>> {
        let scope_cap = |perm: Option<Permission>| -> Option<Permission> {
            let perm = perm?;
            match &by.scope {
                None => Some(perm),
                Some(s) => {
                    if s.projects
                        .as_ref()
                        .is_some_and(|ps| !ps.iter().any(|p| p == project))
                    {
                        None
                    } else {
                        Some(perm.min(s.permission))
                    }
                }
            }
        };
        match (by.kind, by.via) {
            (UserKind::Human, _) => Ok(self.human_permission(&by.user, project).await?),
            (UserKind::Agent, AuthVia::Internal) => Ok(scope_cap(Some(Permission::Write))),
            (UserKind::Agent, _) => {
                let base = match by.acts_for.as_deref().and_then(Owner::from_id) {
                    Some(Owner::User(h)) => self.human_permission(&h, project).await?,
                    Some(Owner::Org(o)) => {
                        let owned = matches!(self.project_owner(project).await?, Some(Owner::Org(x)) if x == o);
                        let shared = self
                            .g_aql(
                                "FOR e IN @@a FILTER e._to == @p AND e._from == @o RETURN e.permission",
                                json!({"@a": ACCESS, "p": format!("{PROJECTS}/{project}"), "o": format!("{ORGS}/{o}")}),
                            )
                            .await?
                            .first()
                            .and_then(Value::as_str)
                            .map(Permission::parse)
                            .transpose()?;
                        let org_ok = self.get_org(&o).await?["archived"] != true;
                        if !org_ok {
                            None
                        } else if owned {
                            Some(Permission::Write)
                        } else {
                            shared
                        }
                    }
                    None => None,
                };
                Ok(scope_cap(base))
            }
        }
    }

    /// Projects the caller can reach, with their permission.
    pub async fn visible_projects(&self, by: &Principal) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for p in self.list_projects().await? {
            let id = p["id"].as_str().unwrap_or_default().to_string();
            if let Some(perm) = self.project_permission(by, &id).await? {
                let mut p = p;
                p["permission"] = json!(perm);
                p["owner"] = self
                    .g_get(PROJECTS, &id)
                    .await?
                    .map(|d| d["owner"].clone())
                    .unwrap_or(Value::Null);
                p.as_object_mut().map(|m| m.remove("active"));
                out.push(p);
            }
        }
        Ok(out)
    }

    /// Create a project owned by the caller (or an org the caller administers).
    pub async fn create_owned_project(
        &self,
        by: &Principal,
        name: &str,
        description: &str,
        org: Option<&str>,
    ) -> Result<Value> {
        if !by.is_human() {
            return Err(StoreError::Invalid(
                "projects are created by signed-in humans".into(),
            ));
        }
        let owner = match org {
            Some(o) => {
                self.require_org_admin(by, o).await?;
                Owner::Org(o.to_string())
            }
            None => Owner::User(by.user.clone()),
        };
        let p = self.create_project(name, description).await?;
        let id = p["id"].as_str().unwrap_or_default().to_string();
        self.set_project_owner(&id, &owner).await?;
        let mut p = p;
        p["owner"] = json!(owner.id());
        p["permission"] = json!(Permission::Admin);
        p.as_object_mut().map(|m| m.remove("active"));
        Ok(p)
    }
}

/// Public view of a token record (never the token itself).
pub fn public_token(t: &Value) -> Value {
    json!({
        "id": t["_key"], "user": t["user"], "name": t["name"], "scope": t["scope"],
        "created_at": t["created_at"], "expires_at": t["expires_at"],
        "last_used_at": t["last_used_at"], "revoked": t["revoked"] == true, "prefix": t["prefix"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_hash_and_prefix() {
        let t = random_token(PAT_PREFIX);
        assert!(t.starts_with("khp_") && t.len() == 68);
        assert_eq!(token_key(&t), token_key(&t));
        assert_ne!(token_key(&t), token_key(&random_token(PAT_PREFIX)));
    }

    #[test]
    fn usernames() {
        assert!(valid_username("alice"));
        assert!(valid_username("bob.smith-2"));
        assert!(!valid_username("A"));
        assert!(!valid_username("1abc"));
        assert!(!valid_username("al ice"));
    }

    #[test]
    fn passwords() {
        let h = hash_password("correct horse").unwrap();
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong horse!", &h));
        assert!(hash_password("short").is_err());
    }

    #[test]
    fn permission_order() {
        assert!(Permission::Admin > Permission::Write && Permission::Write > Permission::Read);
        assert_eq!(Permission::Admin.min(Permission::Read), Permission::Read);
    }
}
