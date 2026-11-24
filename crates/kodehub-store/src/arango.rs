//! Thin ArangoDB HTTP API wrapper (doc 01: no third-party driver).
//!
//! ## Projects (decision D-projects)
//! One ArangoDB database holds every project. Each project owns its own set of collections,
//! physically named `<project>__<collection>` (plus its named graph `<project>__kodehub`), next
//! to the global registry collections (`kodehub_projects`, `kodehub_meta`). Everything above this
//! wrapper keeps using *logical* names (`symbols`, `symbols/rust-x`):
//!
//! - collection arguments (`insert("symbols", ..)`, `begin(&["versions"])`) map to physical names;
//! - AQL collection references are written as `@@symbols` (auto-bound to the active project's
//!   collection), the named graph as `@kodehub_graph`, and literal id prefixes as
//!   `CONCAT(@pfx, 'symbols/', ..)`; `@@c`-style bind values naming a collection are mapped too;
//! - every string in a request body / bind vars that is exactly a logical id (`symbols/x`) is
//!   rewritten to the physical id, and every physical id in a response back to the logical one.
//!
//! In a fresh database no unprefixed project collections exist, so any reference that bypasses
//! this mapping fails loudly ("collection not found") instead of reading another project.

use std::sync::{Arc, RwLock};

use kodehub_core::model::{DOCUMENT_COLLECTIONS, EDGE_COLLECTIONS};
use reqwest::Method;
use serde_json::{json, Value};

use crate::{Result, StoreError};

/// Logical graph name (one named graph per project, physically `<project>__kodehub`).
pub const GRAPH_NAME: &str = "kodehub";
/// Separator between the project id and the logical collection name.
pub const SEP: &str = "__";

tokio::task_local! {
    /// Project selected for the current request (set by the server's auth layer). It takes
    /// precedence over the handle's shared project, so concurrent requests for different
    /// projects never see each other's data.
    pub static REQUEST_PROJECT: String;
}

/// Run `fut` with `project` as the request-scoped project.
pub async fn with_project<F: std::future::Future>(project: String, fut: F) -> F::Output {
    REQUEST_PROJECT.scope(project, fut).await
}

/// The request-scoped project, if any.
pub fn request_project() -> Option<String> {
    REQUEST_PROJECT.try_with(|p| p.clone()).ok()
}

/// Spawn a task that keeps the caller's request-scoped project.
pub fn spawn_scoped<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    match request_project() {
        Some(p) => tokio::spawn(REQUEST_PROJECT.scope(p, fut)),
        None => tokio::spawn(fut),
    }
}

/// True for a project-scoped (logical) collection name.
pub fn is_project_collection(c: &str) -> bool {
    DOCUMENT_COLLECTIONS.contains(&c) || EDGE_COLLECTIONS.contains(&c)
}

/// Valid project id: `[a-z][a-z0-9_]{0,31}`, no `__`, no trailing `_`.
pub fn valid_project_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !id.contains(SEP)
        && !id.ends_with('_')
}

/// ArangoDB document key characters (no `/`, `%`, `?`, `#`): keys are spliced into REST URLs.
pub fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 254
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-:.@()+,=;$!*'".contains(c))
        && k != "."
        && k != ".."
}

pub fn valid_collection(c: &str) -> bool {
    !c.is_empty()
        && c.chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

/// Apply `f` to every string in a JSON value (keys untouched); `None` keeps the string.
fn map_strings(v: &Value, f: &dyn Fn(&str) -> Option<String>) -> Value {
    match v {
        Value::String(s) => Value::String(f(s).unwrap_or_else(|| s.clone())),
        Value::Array(a) => Value::Array(a.iter().map(|x| map_strings(x, f)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), map_strings(x, f)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Names used as `@@name` collection bind parameters in an AQL query.
pub fn collection_bind_names(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = query;
    while let Some(i) = rest.find("@@") {
        rest = &rest[i + 2..];
        let name: String = rest.chars().take_while(|c| is_ident_char(*c)).collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// True when `@name` (as a whole bind parameter, not `@@name`) occurs in the query.
fn uses_param(query: &str, name: &str) -> bool {
    let pat = format!("@{name}");
    let mut start = 0;
    while let Some(i) = query[start..].find(&pat) {
        let at = start + i;
        let before_ok = at == 0 || &query[at - 1..at] != "@";
        let after = query[at + pat.len()..].chars().next();
        if before_ok && !after.is_some_and(is_ident_char) {
            return true;
        }
        start = at + pat.len();
    }
    false
}

fn check(coll: &str, key: &str) -> Result<()> {
    if !valid_collection(coll) {
        return Err(StoreError::Invalid(format!(
            "invalid collection name `{coll}`"
        )));
    }
    if !valid_key(key) {
        return Err(StoreError::Invalid(format!("invalid document key `{key}`")));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ArangoConfig {
    pub url: String,
    pub user: String,
    pub password: String,
    pub database: String,
}

impl ArangoConfig {
    pub fn from_env() -> Self {
        ArangoConfig {
            url: std::env::var("KODEHUB_ARANGO_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8528".into()),
            user: std::env::var("KODEHUB_ARANGO_USER").unwrap_or_else(|_| "root".into()),
            password: std::env::var("KODEHUB_ARANGO_PASSWORD").unwrap_or_else(|_| "kodehub".into()),
            database: std::env::var("KODEHUB_DB").unwrap_or_else(|_| "kodehub".into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Arango {
    http: reqwest::Client,
    pub cfg: ArangoConfig,
    /// Active project id, shared by every clone of this handle (server, runner, agents).
    project: Arc<RwLock<Option<String>>>,
    /// A handle made by `scoped()` is pinned to its project and ignores the request scope.
    pub(crate) pinned: bool,
}

impl Arango {
    pub fn new(cfg: ArangoConfig) -> Self {
        Arango {
            http: reqwest::Client::new(),
            cfg,
            project: Arc::new(RwLock::new(None)),
            pinned: false,
        }
    }

    /// The active project id, if one is selected.
    pub fn project(&self) -> Option<String> {
        if self.pinned {
            return self.project.read().unwrap().clone();
        }
        request_project().or_else(|| self.project.read().unwrap().clone())
    }

    /// Select the active project for every clone of this handle.
    pub fn set_project(&self, id: Option<String>) {
        *self.project.write().unwrap() = id;
    }

    fn prefix(&self) -> Option<String> {
        self.project().map(|p| format!("{p}{SEP}"))
    }

    /// Physical name of a collection (project-scoped names get the active project's prefix).
    pub fn phys(&self, logical: &str) -> String {
        match self.prefix() {
            Some(p) if is_project_collection(logical) => format!("{p}{logical}"),
            _ => logical.to_string(),
        }
    }

    /// Physical name of a collection in a given project.
    pub fn phys_in(project: &str, logical: &str) -> String {
        format!("{project}{SEP}{logical}")
    }

    /// Physical name of the active project's named graph.
    pub fn graph(&self) -> String {
        self.phys_graph()
    }

    fn phys_graph(&self) -> String {
        match self.prefix() {
            Some(p) => format!("{p}{GRAPH_NAME}"),
            None => GRAPH_NAME.to_string(),
        }
    }

    /// Rewrite logical ids (`symbols/x`) in a JSON value to physical ids.
    fn to_phys(&self, v: &Value) -> Value {
        match self.prefix() {
            Some(p) => map_strings(v, &|s| {
                let (c, k) = s.split_once('/')?;
                (is_project_collection(c) && valid_key(k)).then(|| format!("{p}{s}"))
            }),
            None => v.clone(),
        }
    }

    /// Rewrite physical ids of the active project in a JSON value back to logical ids.
    fn to_logical(&self, v: Value) -> Value {
        match self.prefix() {
            Some(p) => map_strings(&v, &|s| {
                let rest = s.strip_prefix(p.as_str())?;
                let (c, _) = rest.split_once('/')?;
                is_project_collection(c).then(|| rest.to_string())
            }),
            None => v,
        }
    }

    /// Bind vars for an AQL query: ids → physical, `@x` collection binds mapped, and the
    /// auto-binds `@@<collection>`, `@kodehub_graph` and `@pfx` added when the query uses them.
    fn aql_bind(&self, query: &str, bind: &Value) -> Value {
        let mut b = match self.to_phys(bind) {
            Value::Object(m) => m,
            Value::Null => serde_json::Map::new(),
            other => return other,
        };
        for (k, v) in b.iter_mut() {
            if k.starts_with('@') {
                if let Some(c) = v.as_str() {
                    *v = json!(self.phys(c));
                }
            }
        }
        for name in collection_bind_names(query) {
            let key = format!("@{name}");
            if is_project_collection(&name) && !b.contains_key(&key) {
                b.insert(key, json!(self.phys(&name)));
            }
        }
        if uses_param(query, "kodehub_graph") && !b.contains_key("kodehub_graph") {
            b.insert("kodehub_graph".into(), json!(self.phys_graph()));
        }
        if uses_param(query, "pfx") && !b.contains_key("pfx") {
            b.insert("pfx".into(), json!(self.prefix().unwrap_or_default()));
        }
        Value::Object(b)
    }

    fn db_url(&self, path: &str) -> String {
        format!("{}/_db/{}{}", self.cfg.url, self.cfg.database, path)
    }

    /// Send with id mapping (request body → physical ids, response → logical ids).
    async fn send(
        &self,
        method: Method,
        url: String,
        body: Option<&Value>,
        trx: Option<&str>,
    ) -> Result<Value> {
        let body = body.map(|b| self.to_phys(b));
        let v = self.send_raw(method, url, body.as_ref(), trx).await?;
        Ok(self.to_logical(v))
    }

    /// Send without any id mapping (registry, migration, admin calls).
    pub(crate) async fn send_raw(
        &self,
        method: Method,
        url: String,
        body: Option<&Value>,
        trx: Option<&str>,
    ) -> Result<Value> {
        let mut req = self
            .http
            .request(method, url)
            .basic_auth(&self.cfg.user, Some(&self.cfg.password));
        if let Some(t) = trx {
            req = req.header("x-arango-trx-id", t);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let text = resp.text().await?;
        let v: Value = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        if status >= 400 || v.get("error").and_then(Value::as_bool) == Some(true) {
            return Err(StoreError::Arango {
                code: status,
                num: v.get("errorNum").and_then(Value::as_i64).unwrap_or(0),
                message: v
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
            });
        }
        Ok(v)
    }

    pub async fn db(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        self.send(method, self.db_url(path), body, None).await
    }

    /// Database-level call without id mapping (schema/admin endpoints).
    pub async fn db_raw(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        self.send_raw(method, self.db_url(path), body, None).await
    }

    /// AQL without any name or id mapping (registry and legacy-migration queries).
    pub async fn aql_raw(&self, query: &str, bind: Value) -> Result<Vec<Value>> {
        let body = json!({"query": query, "bindVars": bind, "batchSize": 1000});
        let mut v = self
            .send_raw(Method::POST, self.db_url("/_api/cursor"), Some(&body), None)
            .await?;
        let mut out: Vec<Value> = v["result"].as_array().cloned().unwrap_or_default();
        while v["hasMore"].as_bool() == Some(true) {
            let id = v["id"].as_str().unwrap_or_default().to_string();
            v = self
                .send_raw(
                    Method::POST,
                    self.db_url(&format!("/_api/cursor/{id}")),
                    None,
                    None,
                )
                .await?;
            out.extend(v["result"].as_array().cloned().unwrap_or_default());
        }
        Ok(out)
    }

    pub async fn version(&self) -> Result<Value> {
        self.send(
            Method::GET,
            format!("{}/_api/version", self.cfg.url),
            None,
            None,
        )
        .await
    }

    pub async fn ensure_database(&self) -> Result<()> {
        let url = format!("{}/_db/_system/_api/database", self.cfg.url);
        match self
            .send(
                Method::POST,
                url,
                Some(&json!({"name": self.cfg.database})),
                None,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(StoreError::Arango { num: 1207, .. }) => Ok(()), // duplicate name
            Err(e) => Err(e),
        }
    }

    pub async fn drop_database(&self) -> Result<()> {
        let url = format!(
            "{}/_db/_system/_api/database/{}",
            self.cfg.url, self.cfg.database
        );
        match self.send(Method::DELETE, url, None, None).await {
            Ok(_) => Ok(()),
            Err(StoreError::Arango { num: 1228, .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Run AQL, following cursors until exhausted.
    pub async fn aql(&self, query: &str, bind: Value, trx: Option<&str>) -> Result<Vec<Value>> {
        let bind = self.aql_bind(query, &bind);
        // bind vars are already physical: send them unmapped, map only the results
        let body = json!({"query": query, "bindVars": bind, "batchSize": 1000});
        let mut v = self
            .send_raw(Method::POST, self.db_url("/_api/cursor"), Some(&body), trx)
            .await
            .map(|v| self.to_logical(v))?;
        let mut out: Vec<Value> = v["result"].as_array().cloned().unwrap_or_default();
        while v["hasMore"].as_bool() == Some(true) {
            let id = v["id"].as_str().unwrap_or_default().to_string();
            v = self
                .send(
                    Method::POST,
                    self.db_url(&format!("/_api/cursor/{id}")),
                    None,
                    trx,
                )
                .await?;
            out.extend(v["result"].as_array().cloned().unwrap_or_default());
        }
        Ok(out)
    }

    pub async fn begin(&self, write: &[&str], read: &[&str]) -> Result<String> {
        let w: Vec<String> = write.iter().map(|c| self.phys(c)).collect();
        let r: Vec<String> = read.iter().map(|c| self.phys(c)).collect();
        let body = json!({"collections": {"write": w, "read": r}, "waitForSync": false});
        let v = self
            .send(
                Method::POST,
                self.db_url("/_api/transaction/begin"),
                Some(&body),
                None,
            )
            .await?;
        Ok(v["result"]["id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn commit(&self, trx: &str) -> Result<()> {
        self.send(
            Method::PUT,
            self.db_url(&format!("/_api/transaction/{trx}")),
            None,
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn abort(&self, trx: &str) -> Result<()> {
        self.send(
            Method::DELETE,
            self.db_url(&format!("/_api/transaction/{trx}")),
            None,
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn insert(&self, coll: &str, doc: &Value, trx: Option<&str>) -> Result<Value> {
        check(coll, "x")?;
        let coll = self.phys(coll);
        self.send(
            Method::POST,
            self.db_url(&format!("/_api/document/{coll}?returnNew=true")),
            Some(doc),
            trx,
        )
        .await
        .map(|v| v["new"].clone())
    }

    /// Insert, ignoring an existing document with the same key.
    pub async fn insert_ignore(&self, coll: &str, doc: &Value, trx: Option<&str>) -> Result<()> {
        check(coll, "x")?;
        let coll = self.phys(coll);
        self.send(
            Method::POST,
            self.db_url(&format!("/_api/document/{coll}?overwriteMode=ignore")),
            Some(doc),
            trx,
        )
        .await
        .map(|_| ())
    }

    pub async fn get(&self, coll: &str, key: &str, trx: Option<&str>) -> Result<Value> {
        check(coll, key)?;
        let pcoll = self.phys(coll);
        self.send(
            Method::GET,
            self.db_url(&format!("/_api/document/{pcoll}/{key}")),
            None,
            trx,
        )
        .await
        .map_err(|e| match e {
            StoreError::Arango { num: 1202, .. } => StoreError::NotFound(format!("{coll}/{key}")),
            other => other,
        })
    }

    pub async fn patch(
        &self,
        coll: &str,
        key: &str,
        doc: &Value,
        trx: Option<&str>,
    ) -> Result<Value> {
        check(coll, key)?;
        let coll = self.phys(coll);
        self.send(
            Method::PATCH,
            self.db_url(&format!(
                "/_api/document/{coll}/{key}?returnNew=true&mergeObjects=false"
            )),
            Some(doc),
            trx,
        )
        .await
        .map(|v| v["new"].clone())
    }

    pub async fn remove(&self, coll: &str, key: &str, trx: Option<&str>) -> Result<()> {
        check(coll, key)?;
        let coll = self.phys(coll);
        self.send(
            Method::DELETE,
            self.db_url(&format!("/_api/document/{coll}/{key}")),
            None,
            trx,
        )
        .await
        .map(|_| ())
    }
}

impl Arango {
    /// Insert or replace by `_key`.
    pub async fn upsert(&self, coll: &str, doc: &Value, trx: Option<&str>) -> Result<()> {
        check(coll, "x")?;
        let coll = self.phys(coll);
        self.send(
            Method::POST,
            self.db_url(&format!("/_api/document/{coll}?overwriteMode=replace")),
            Some(doc),
            trx,
        )
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_cannot_traverse_paths() {
        assert!(valid_key("r1-generic-failure"));
        assert!(valid_key("UserStore-find_user"));
        assert!(!valid_key("../versions/abc"));
        assert!(!valid_key("a/b"));
        assert!(!valid_key("a%2Fb"));
        assert!(!valid_key("a?x=1"));
        assert!(!valid_key(".."));
        assert!(!valid_collection("versions/x"));
    }
}
