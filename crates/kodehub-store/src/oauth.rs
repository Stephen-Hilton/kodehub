//! OAuth 2.1 for remote MCP (spec §13A.2): dynamic client registration, authorization codes
//! with PKCE (S256), and access + refresh tokens issued to an **agent user** that acts for the
//! approving human — the assistant never acts as the human. Codes and tokens are stored only as
//! hashes; access tokens are ordinary personal access tokens, so revocation and scoping work the
//! same way as everywhere else.

use base64::Engine as _;
use kodehub_core::model::now_iso;
use serde_json::{json, Value};
use sha2::Digest;

use crate::identity::{random_token, token_key, AuthVia, Permission, Principal, UserKind, TOKENS};
use crate::{Result, Store, StoreError};

pub const CLIENTS: &str = "kodehub_oauth_clients";
pub const CODES: &str = "kodehub_oauth_codes";

const ACCESS_TTL_SECS: i64 = 3600;
const REFRESH_TTL_DAYS: i64 = 30;

fn after(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn expired(at: &Value) -> bool {
    at.as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|t| t < chrono::Utc::now())
}

/// PKCE S256: base64url(sha256(verifier)) without padding.
pub fn s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()))
}

fn redirect_ok(uri: &str) -> bool {
    // https anywhere, http only for loopback (native / CLI clients)
    uri.starts_with("https://")
        || uri.starts_with("http://127.0.0.1")
        || uri.starts_with("http://localhost")
        || uri.starts_with("http://[::1]")
}

impl Store {
    /// RFC 7591 dynamic client registration (public clients, PKCE required).
    pub async fn oauth_register(&self, name: &str, redirect_uris: &[String]) -> Result<Value> {
        if redirect_uris.is_empty() || !redirect_uris.iter().all(|u| redirect_ok(u)) {
            return Err(StoreError::Invalid(
                "redirect_uris must be https, or http on a loopback address".into(),
            ));
        }
        let id = random_token("khc_");
        let doc = json!({"_key": token_key(&id), "client_id": id, "client_name": name.trim(),
                         "redirect_uris": redirect_uris, "created_at": now_iso()});
        self.g_insert(CLIENTS, &doc).await?;
        Ok(
            json!({"client_id": id, "client_name": name.trim(), "redirect_uris": redirect_uris,
                  "token_endpoint_auth_method": "none", "grant_types": ["authorization_code", "refresh_token"],
                  "response_types": ["code"]}),
        )
    }

    pub async fn oauth_client(&self, client_id: &str) -> Result<Value> {
        self.g_get(CLIENTS, &token_key(client_id))
            .await?
            .ok_or_else(|| StoreError::Invalid("unknown client_id".into()))
    }

    /// The human approved: issue a one-time code bound to the client, redirect and PKCE challenge.
    pub async fn oauth_code(
        &self,
        client_id: &str,
        human: &str,
        redirect_uri: &str,
        challenge: &str,
        method: &str,
    ) -> Result<String> {
        let c = self.oauth_client(client_id).await?;
        if !c["redirect_uris"]
            .as_array()
            .is_some_and(|a| a.iter().any(|u| u == redirect_uri))
        {
            return Err(StoreError::Invalid(
                "redirect_uri is not registered for this client".into(),
            ));
        }
        if method != "S256" || challenge.len() < 43 {
            return Err(StoreError::Invalid("PKCE with S256 is required".into()));
        }
        let code = random_token("khz_");
        self.g_insert(
            CODES,
            &json!({"_key": token_key(&code), "client_id": client_id, "user": human, "redirect_uri": redirect_uri,
                    "challenge": challenge, "expires_at": after(120), "used": false, "created_at": now_iso()}),
        )
        .await?;
        Ok(code)
    }

    /// The agent user for one human + one client (created on first approval).
    async fn connector_agent(&self, human: &str, client: &Value) -> Result<String> {
        let name = format!(
            "{} (MCP)",
            client["client_name"].as_str().unwrap_or("assistant")
        );
        let p = Principal {
            user: human.to_string(),
            kind: UserKind::Human,
            acts_for: None,
            via: AuthVia::Session,
            scope: None,
            is_admin: false,
            display_name: human.to_string(),
            token_id: None,
        };
        match self.create_agent_user(&p, &name, None).await {
            Ok(u) => Ok(u["key"].as_str().unwrap_or_default().to_string()),
            Err(StoreError::Conflict(_)) => Ok(format!("agent-{human}-{}", crate::slug(&name))
                .chars()
                .take(60)
                .collect()),
            Err(e) => Err(e),
        }
    }

    async fn issue_pair(&self, agent: &str, human: &str, client_id: &str) -> Result<Value> {
        let access = random_token("khp_");
        self.g_insert(
            TOKENS,
            &json!({"_key": token_key(&access), "user": agent, "name": "oauth access",
                    "scope": {"projects": null, "permission": Permission::Write},
                    "created_at": now_iso(), "created_by": format!("kodehub_users/{human}"),
                    "expires_at": after(ACCESS_TTL_SECS), "revoked": false, "prefix": &access[..12],
                    "oauth_client": client_id}),
        )
        .await?;
        let refresh = random_token("khr_");
        self.g_insert(
            TOKENS,
            &json!({"_key": token_key(&refresh), "kind": "refresh", "user": agent, "human": human,
                    "oauth_client": client_id, "created_at": now_iso(),
                    "expires_at": after(REFRESH_TTL_DAYS * 86400), "revoked": false}),
        )
        .await?;
        Ok(
            json!({"access_token": access, "token_type": "Bearer", "expires_in": ACCESS_TTL_SECS,
                  "refresh_token": refresh, "scope": "kodehub"}),
        )
    }

    /// `grant_type=authorization_code`.
    pub async fn oauth_exchange(
        &self,
        code: &str,
        client_id: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<Value> {
        let bad = || StoreError::Invalid("invalid_grant".into());
        let k = token_key(code);
        let c = self.g_get(CODES, &k).await?.ok_or_else(bad)?;
        if c["used"] == true
            || expired(&c["expires_at"])
            || c["client_id"] != client_id
            || c["redirect_uri"] != redirect_uri
        {
            return Err(bad());
        }
        if s256(verifier) != c["challenge"].as_str().unwrap_or("") {
            return Err(bad());
        }
        self.g_patch(CODES, &k, &json!({"used": true})).await?;
        let human = c["user"].as_str().unwrap_or_default().to_string();
        let client = self.oauth_client(client_id).await?;
        let agent = self.connector_agent(&human, &client).await?;
        self.issue_pair(&agent, &human, client_id).await
    }

    /// `grant_type=refresh_token` (rotating: the old refresh token stops working).
    pub async fn oauth_refresh(&self, refresh: &str, client_id: &str) -> Result<Value> {
        let bad = || StoreError::Invalid("invalid_grant".into());
        let k = token_key(refresh);
        let t = self.g_get(TOKENS, &k).await?.ok_or_else(bad)?;
        if t["kind"] != "refresh"
            || t["revoked"] == true
            || expired(&t["expires_at"])
            || t["oauth_client"] != client_id
        {
            return Err(bad());
        }
        // the human (and the agent) must still be allowed in
        let agent = t["user"].as_str().unwrap_or_default().to_string();
        let human = t["human"].as_str().unwrap_or_default().to_string();
        for u in [&agent, &human] {
            if self.get_user(u).await?["disabled"] == true {
                return Err(bad());
            }
        }
        self.g_patch(
            TOKENS,
            &k,
            &json!({"revoked": true, "rotated_at": now_iso()}),
        )
        .await?;
        self.issue_pair(&agent, &human, client_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_matches_rfc7636_example() {
        assert_eq!(
            s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(redirect_ok("https://claude.ai/api/mcp/auth_callback"));
        assert!(redirect_ok("http://127.0.0.1:5555/cb"));
        assert!(!redirect_ok("http://evil.example/cb"));
    }
}
