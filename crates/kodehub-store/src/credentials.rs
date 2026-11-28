//! Model and Git credentials (spec §12.1, §13.3): layered, stored encrypted, never logged.
//!
//! Ciphertext uses ChaCha20-Poly1305 with a key that never enters the graph: the
//! `KODEHUB_SECRET_KEY` environment variable (base64, 32 bytes; a Kubernetes Secret in the
//! cluster) or, on a laptop, `$KODEHUB_HOME/secret.key` (created 0600 on first use).
//!
//! Rules enforced here (not only in the UI):
//! - a subscription token can be stored only at `user:<self>` and is used only for projects
//!   that user owns directly; orgs and org-owned projects use API keys;
//! - the `claude_code` backend uses only subscription tokens and the `api` backend only API
//!   keys, so a subscription token can never reach the Messages API.

use base64::Engine as _;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use kodehub_core::model::now_iso;
use rand::RngCore;
use reqwest::Method;
use serde::Serialize;
use serde_json::{json, Value};

use crate::identity::{Owner, Principal};
use crate::settings::{human_of, parse_scope};
use crate::{Result, Store, StoreError};

pub const CREDENTIALS: &str = "secrets";
/// Edges from a secret to what it unlocks (D-061): `secrets → users | orgs | projects |
/// remote_agents | deployers`.
pub const SECURES: &str = "secures";

/// The holder a credential scope names (`user:x` → `users/x`).
fn holder_of(scope: &str) -> Option<String> {
    let (k, id) = scope.split_once(':')?;
    let coll = match k {
        "user" => crate::identity::USERS,
        "org" => crate::identity::ORGS,
        "project" => crate::schema::PROJECTS,
        _ => return None,
    };
    Some(format!("{coll}/{id}"))
}

pub const KINDS: &[&str] = &["api_key", "subscription_token", "git_token"];

/// A decrypted secret. `Debug` and `Serialize` never show the value.
#[derive(Clone)]
pub struct Secret {
    pub kind: String,
    pub provider: String,
    pub source: String,
    value: String,
}

impl Secret {
    pub fn expose(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Secret({} {} from {}, value redacted)",
            self.provider, self.kind, self.source
        )
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        json!({"provider": self.provider, "kind": self.kind, "source": self.source, "value": "[redacted]"})
            .serialize(s)
    }
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Load (or create) the encryption key.
pub fn secret_key() -> Result<[u8; 32]> {
    let bad = |m: String| StoreError::Invalid(format!("secret key: {m}"));
    if let Ok(v) = std::env::var("KODEHUB_SECRET_KEY") {
        let raw = b64().decode(v.trim()).map_err(|e| bad(e.to_string()))?;
        return raw
            .try_into()
            .map_err(|_| bad("KODEHUB_SECRET_KEY must decode to 32 bytes".into()));
    }
    let path = kodehub_core::config::kodehub_home().join("secret.key");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let raw = b64().decode(s.trim()).map_err(|e| bad(e.to_string()))?;
        return raw
            .try_into()
            .map_err(|_| bad(format!("{} must hold 32 bytes", path.display())));
    }
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| bad(e.to_string()))?;
    }
    std::fs::write(&path, b64().encode(k)).map_err(|e| bad(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(k)
}

fn encrypt(plain: &str) -> Result<(String, String)> {
    let key = secret_key()?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let mut n = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut n);
    let ct = cipher
        .encrypt(Nonce::from_slice(&n), plain.as_bytes())
        .map_err(|_| StoreError::Invalid("encryption failed".into()))?;
    Ok((b64().encode(n), b64().encode(ct)))
}

fn decrypt(nonce: &str, ct: &str) -> Result<String> {
    let key = secret_key()?;
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let n = b64()
        .decode(nonce)
        .map_err(|_| StoreError::Invalid("bad nonce".into()))?;
    let c = b64()
        .decode(ct)
        .map_err(|_| StoreError::Invalid("bad ciphertext".into()))?;
    let p = cipher
        .decrypt(Nonce::from_slice(&n), c.as_ref())
        .map_err(|_| StoreError::Invalid("credential cannot be decrypted with this key".into()))?;
    String::from_utf8(p).map_err(|_| StoreError::Invalid("credential is not text".into()))
}

fn cred_key(scope: &str, provider: &str, kind: &str) -> String {
    blake3::hash(format!("{scope}|{provider}|{kind}").as_bytes()).to_hex()[..32].to_string()
}

fn public(c: &Value) -> Value {
    json!({"id": c["_key"], "scope": c["scope"], "provider": c["provider"], "kind": c["kind"],
           "label": c["label"], "fingerprint": c["fingerprint"],
           "created_at": c["created_at"], "created_by": c["created_by"]})
}

impl Store {
    /// Store a credential at a scope (encrypted). Returns its public record.
    pub async fn put_credential(
        &self,
        by: &Principal,
        scope: &str,
        provider: &str,
        kind: &str,
        secret: &str,
        label: &str,
    ) -> Result<Value> {
        let (s_kind, s_id) = parse_scope(scope)?;
        if !KINDS.contains(&kind) {
            return Err(StoreError::Invalid(format!(
                "credential kind must be one of {KINDS:?}"
            )));
        }
        if s_kind == "app" {
            return Err(StoreError::Invalid(
                "credentials are set at user, org or project scope".into(),
            ));
        }
        if kind == "subscription_token" && !(s_kind == "user" && s_id == by.user) {
            return Err(StoreError::Invalid(
                "subscription tokens are personal: set one on your own user only. Organizations \
                 and organization projects must use API keys."
                    .into(),
            ));
        }
        if kind == "subscription_token" && secret.trim().starts_with("sk-ant-api") {
            return Err(StoreError::Invalid(
                "that looks like an API key, not a subscription token".into(),
            ));
        }
        if kind == "api_key" && secret.trim().starts_with("sk-ant-oat") {
            return Err(StoreError::Invalid(
                "that looks like a subscription token, not an API key".into(),
            ));
        }
        if !self.can_manage_scope(by, scope).await? {
            return Err(StoreError::Invalid(format!(
                "you may not set credentials at {scope}"
            )));
        }
        let secret = secret.trim();
        if secret.is_empty() {
            return Err(StoreError::Invalid("the secret is empty".into()));
        }
        let (nonce, ct) = encrypt(secret)?;
        let doc = json!({
            "_key": cred_key(scope, provider, kind), "scope": scope, "provider": provider,
            "kind": kind, "label": label.trim(), "nonce": nonce, "ciphertext": ct,
            "fingerprint": &blake3::hash(secret.as_bytes()).to_hex()[..8],
            "created_at": now_iso(), "created_by": by.id(),
        });
        let v = self
            .db
            .db_raw(
                Method::POST,
                &format!("/_api/document/{CREDENTIALS}?overwriteMode=replace&returnNew=true"),
                Some(&doc),
            )
            .await?;
        if let Some(holder) = holder_of(scope) {
            let from = format!("{CREDENTIALS}/{}", cred_key(scope, provider, kind));
            self.db
                .db_raw(
                    Method::POST,
                    &format!("/_api/document/{SECURES}?overwriteMode=ignore"),
                    Some(&json!({"_key": crate::edge_key(SECURES, &from, &holder),
                                 "_from": from, "_to": holder, "created_at": now_iso()})),
                )
                .await?;
        }
        Ok(public(&v["new"]))
    }

    pub async fn delete_credential(&self, by: &Principal, id: &str) -> Result<()> {
        let c = self
            .g_get(CREDENTIALS, id)
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("credential {id}")))?;
        if !self
            .can_manage_scope(by, c["scope"].as_str().unwrap_or_default())
            .await?
        {
            return Err(StoreError::Invalid("not allowed".into()));
        }
        self.db
            .db_raw(
                Method::DELETE,
                &format!("/_api/document/{CREDENTIALS}/{id}"),
                None,
            )
            .await?;
        self.g_aql(
            "FOR e IN @@s FILTER e._from == @f REMOVE e IN @@s",
            json!({"@s": SECURES, "f": format!("{CREDENTIALS}/{id}")}),
        )
        .await?;
        Ok(())
    }

    /// Public records of the credentials stored at a scope.
    pub async fn list_credentials(&self, by: &Principal, scope: &str) -> Result<Vec<Value>> {
        if !self.can_view_scope(by, scope).await? {
            return Err(StoreError::Invalid("not allowed".into()));
        }
        Ok(self
            .g_aql(
                "FOR c IN @@c FILTER c.scope == @s SORT c.provider, c.kind RETURN c",
                json!({"@c": CREDENTIALS, "s": scope}),
            )
            .await?
            .iter()
            .map(public)
            .collect())
    }

    async fn credential_at(
        &self,
        scope: &str,
        provider: &str,
        kind: &str,
    ) -> Result<Option<Secret>> {
        let Some(c) = self
            .g_get(CREDENTIALS, &cred_key(scope, provider, kind))
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(Secret {
            kind: kind.to_string(),
            provider: provider.to_string(),
            source: scope.to_string(),
            value: decrypt(
                c["nonce"].as_str().unwrap_or_default(),
                c["ciphertext"].as_str().unwrap_or_default(),
            )?,
        }))
    }

    /// The credential an agent backend may use for work in `project` on behalf of `actor`
    /// (`None` = the project owner). `backend` is `claude_code` (subscription token) or `api`
    /// (API key). Lookup order, most specific first: the acting user, the project, the owner.
    pub async fn resolve_model_credential(
        &self,
        project: &str,
        actor: Option<&Principal>,
        backend: &str,
    ) -> Result<Option<Secret>> {
        let owner = self.project_owner(project).await?;
        match backend {
            "claude_code" => {
                // personal only: the owner of a user-owned project
                let Some(Owner::User(u)) = owner else {
                    return Ok(None);
                };
                if let Some(a) = actor {
                    if human_of(a) != u {
                        return Ok(None);
                    }
                }
                self.credential_at(&format!("user:{u}"), "anthropic", "subscription_token")
                    .await
            }
            "api" => {
                let mut scopes = Vec::new();
                if let Some(a) = actor {
                    scopes.push(format!("user:{}", human_of(a)));
                }
                scopes.push(format!("project:{project}"));
                match owner {
                    Some(Owner::User(u)) => scopes.push(format!("user:{u}")),
                    Some(Owner::Org(o)) => scopes.push(format!("org:{o}")),
                    None => {}
                }
                for s in scopes {
                    if let Some(sec) = self.credential_at(&s, "anthropic", "api_key").await? {
                        return Ok(Some(sec));
                    }
                }
                Ok(None)
            }
            other => Err(StoreError::Invalid(format!(
                "backend `{other}` takes no model credential"
            ))),
        }
    }

    /// Git credential for a project (acting user, project, owner).
    pub async fn resolve_git_credential(
        &self,
        project: &str,
        actor: Option<&Principal>,
    ) -> Result<Option<Secret>> {
        let mut scopes = Vec::new();
        if let Some(a) = actor {
            scopes.push(format!("user:{}", human_of(a)));
        }
        scopes.push(format!("project:{project}"));
        match self.project_owner(project).await? {
            Some(Owner::User(u)) => scopes.push(format!("user:{u}")),
            Some(Owner::Org(o)) => scopes.push(format!("org:{o}")),
            None => {}
        }
        for s in scopes {
            if let Some(sec) = self.credential_at(&s, "git", "git_token").await? {
                return Ok(Some(sec));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_redaction() {
        let dir = std::env::temp_dir().join(format!("kh-cred-{}", uuid::Uuid::new_v4().simple()));
        std::env::set_var("KODEHUB_HOME", &dir);
        let (n, c) = encrypt("sk-ant-api03-secret").unwrap();
        assert!(!c.contains("secret"));
        assert_eq!(decrypt(&n, &c).unwrap(), "sk-ant-api03-secret");
        let s = Secret {
            kind: "api_key".into(),
            provider: "anthropic".into(),
            source: "user:a".into(),
            value: "sk-ant-api03-secret".into(),
        };
        assert!(!format!("{s:?}").contains("secret-"));
        assert!(!serde_json::to_string(&s).unwrap().contains("api03"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
