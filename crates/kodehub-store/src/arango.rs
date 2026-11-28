//! Thin ArangoDB HTTP API wrapper (doc 01: no third-party driver).
//!
//! ## Projects share one set of collections (decision D-055)
//! Every project uses the same collections and the one named graph `kodehub`. Project-scoped
//! documents and edges carry a `project` attribute, and their keys start with the project id
//! and a dot (`<project>.<local>`, see [`qualify`]). This wrapper enforces it for the request's
//! project (the request scope, else the handle's active project):
//!
//! - inserts into a project collection get `project` set; a `_key`, `_from` or `_to` that
//!   belongs to another project (or to none) is refused, and a missing `_key` is generated;
//! - `get`, `patch` and `remove` refuse a key of another project (`NotFound`);
//! - in AQL, every scan `FOR x IN @@coll` over a project collection is filtered to the
//!   project (`FILTER x.project == @kh_project` is added), `@@coll` binds are filled in from the
//!   name, `@kodehub_graph` names the graph, and a bind value that is an unqualified or foreign
//!   project id (`code/x`) is refused so a forgotten qualification fails loudly in tests;
//! - patches can never move a document to another project.
//!
//! Traversals need no filter: edges never cross projects, so a walk that starts inside a
//! project stays there. Global collections (projects, users, work items, …) are untouched.

use std::sync::{Arc, RwLock};

use kodehub_core::model::{DOCUMENT_COLLECTIONS, EDGE_COLLECTIONS};
use reqwest::Method;
use serde_json::{json, Value};

use crate::{Result, StoreError};

/// The one named graph.
pub const GRAPH_NAME: &str = "kodehub";
/// Separator between the project id and the local part of a project-scoped key.
pub const KEY_SEP: char = '.';
/// Bind parameter the wrapper adds for the project filter.
pub const PROJECT_BIND: &str = "kh_project";

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

/// True for a project-scoped collection name.
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
        && !id.contains("__")
        && !id.ends_with('_')
}

/// A project-scoped key: `<project>.<local>`. A local part that is already qualified for the
/// project is returned unchanged.
pub fn qualify(project: &str, local: &str) -> String {
    match local.strip_prefix(project) {
        Some(rest) if rest.starts_with(KEY_SEP) => local.to_string(),
        _ => format!("{project}{KEY_SEP}{local}"),
    }
}

/// The project a project-scoped key belongs to (`shop.rust-x` → `shop`).
pub fn key_project(key: &str) -> Option<&str> {
    key.split_once(KEY_SEP)
        .map(|(p, _)| p)
        .filter(|p| valid_project_id(p))
}

/// The local part of a project-scoped key (`shop.rust-x` → `rust-x`).
pub fn local_key(key: &str) -> &str {
    match key.split_once(KEY_SEP) {
        Some((p, rest)) if valid_project_id(p) => rest,
        _ => key,
    }
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

/// Add `FILTER <var>.project == @kh_project` after every `FOR <var> IN @@<bind>` whose bound
/// collection is project-scoped. `coll_of` maps a bind name to its collection.
pub fn inject_project_filters(query: &str, coll_of: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(query.len() + 64);
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < query.len() {
        // match `FOR` at a word boundary
        let at_word = i == 0 || !is_ident_char(bytes[i - 1] as char);
        if at_word && bytes.len() >= i + 4 && bytes[i..i + 3].eq_ignore_ascii_case(b"FOR") {
            if let Some((len, var, bind)) = parse_for_in_bind(&query[i..]) {
                let clause = &query[i..i + len];
                out.push_str(clause);
                if coll_of(&bind).is_some_and(|c| is_project_collection(&c)) {
                    out.push_str(&format!(" FILTER {var}.project == @{PROJECT_BIND}"));
                }
                i += len;
                continue;
            }
        }
        let ch = query[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Parse `FOR <var> IN @@<bind>` at the start of `s`: (length, var, bind).
fn parse_for_in_bind(s: &str) -> Option<(usize, String, String)> {
    let mut idx = 3; // after FOR
    let ws = |s: &str, from: usize| -> usize {
        from + s[from..]
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>()
    };
    let after_for = ws(s, idx);
    if after_for == idx {
        return None;
    }
    idx = after_for;
    let var: String = s[idx..].chars().take_while(|c| is_ident_char(*c)).collect();
    if var.is_empty() {
        return None;
    }
    idx += var.len();
    let after_var = ws(s, idx);
    if after_var == idx || !s[after_var..].to_ascii_uppercase().starts_with("IN") {
        return None;
    }
    idx = after_var + 2;
    let after_in = ws(s, idx);
    if after_in == idx || !s[after_in..].starts_with("@@") {
        return None;
    }
    idx = after_in + 2;
    let bind: String = s[idx..].chars().take_while(|c| is_ident_char(*c)).collect();
    if bind.is_empty() {
        return None;
    }
    idx += bind.len();
    Some((idx, var, bind))
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

    /// The active project, or an error when none is selected.
    pub fn require_project(&self) -> Result<String> {
        self.project()
            .ok_or_else(|| StoreError::Invalid("no project selected".into()))
    }

    /// The active project's key for a local part (`rust-x` → `shop.rust-x`).
    pub fn pkey(&self, local: &str) -> Result<String> {
        Ok(qualify(&self.require_project()?, local))
    }

    /// The active project's id for a collection and local key (`code`, `x` → `code/shop.x`).
    pub fn pid(&self, coll: &str, local: &str) -> Result<String> {
        Ok(format!("{coll}/{}", self.pkey(local)?))
    }

    /// Qualify an id given by a caller: `code/x` → `code/<project>.x` for a project collection
    /// whose key has no project yet; anything else is returned unchanged.
    pub fn qid(&self, id: &str) -> Result<String> {
        match id.split_once('/') {
            Some((coll, key)) if is_project_collection(coll) && !key.contains(KEY_SEP) => {
                Ok(format!("{coll}/{}", self.pkey(key)?))
            }
            _ => Ok(id.to_string()),
        }
    }

    /// The name of the named graph.
    pub fn graph(&self) -> String {
        GRAPH_NAME.to_string()
    }

    /// The key a direct lookup (`get`, `patch`, `remove`) uses: a local key (no `.`) of a
    /// project collection is qualified for the active project. Inserts never do this — they
    /// need the qualified key up front so the caller keeps using the same value.
    fn lookup_key(&self, coll: &str, key: &str) -> Result<String> {
        if is_project_collection(coll) && !key.contains(KEY_SEP) {
            return self.pkey(key);
        }
        Ok(key.to_string())
    }

    /// A key in a project collection must belong to the active project.
    fn own_key(&self, coll: &str, key: &str) -> Result<bool> {
        if !is_project_collection(coll) {
            return Ok(true);
        }
        let p = self.require_project()?;
        Ok(key_project(key) == Some(p.as_str()))
    }

    /// Check one `collection/key` id: a project collection's id must be the active project's.
    /// The project's own registry node (`projects/<p>`) is allowed; other global ids pass.
    fn check_id(&self, id: &str, what: &str) -> Result<()> {
        let Some((coll, key)) = id.split_once('/') else {
            return Err(StoreError::Invalid(format!("{what} `{id}` is not an id")));
        };
        if is_project_collection(coll) && !self.own_key(coll, key)? {
            return Err(StoreError::Invariant(format!(
                "{what} `{id}` does not belong to project `{}` (hard rule 9)",
                self.project().unwrap_or_default()
            )));
        }
        Ok(())
    }

    /// Prepare a document for insertion into `coll`: stamp `project`, check or generate the
    /// key, and check edge endpoints.
    fn prepare(&self, coll: &str, doc: &Value) -> Result<Value> {
        if !is_project_collection(coll) {
            return Ok(doc.clone());
        }
        let p = self.require_project()?;
        let mut d = doc.clone();
        let obj = d
            .as_object_mut()
            .ok_or_else(|| StoreError::Invalid(format!("a `{coll}` document must be an object")))?;
        match obj.get("_key").and_then(Value::as_str) {
            Some(k) => {
                if key_project(k) != Some(p.as_str()) {
                    return Err(StoreError::Invariant(format!(
                        "key `{coll}/{k}` does not belong to project `{p}` (hard rule 9)"
                    )));
                }
            }
            None => {
                let k = qualify(&p, &uuid::Uuid::new_v4().simple().to_string()[..16]);
                obj.insert("_key".into(), json!(k));
            }
        }
        if let Some(other) = obj.get("project").and_then(Value::as_str) {
            if other != p {
                return Err(StoreError::Invariant(format!(
                    "document for project `{other}` written in project `{p}`"
                )));
            }
        }
        obj.insert("project".into(), json!(p));
        for end in ["_from", "_to"] {
            if let Some(id) = obj.get(end).and_then(Value::as_str) {
                let id = id.to_string();
                self.check_id(&id, end)?;
            }
        }
        Ok(d)
    }

    /// Check bind values: a string (or list of strings) shaped like a project id (`code/x`)
    /// must be qualified for the active project. Catches forgotten qualifications early.
    /// Objects are not inspected: they carry content (bodies, outputs) that may contain slashes.
    fn check_bind_ids(&self, v: &Value) -> Result<()> {
        match v {
            Value::String(s) => {
                if let Some((coll, key)) = s.split_once('/') {
                    if is_project_collection(coll) && valid_key(key) {
                        let p = self.require_project()?;
                        if key_project(key) != Some(p.as_str()) {
                            return Err(StoreError::Invariant(format!(
                                "bind value `{s}` is not an id of project `{p}`"
                            )));
                        }
                    }
                }
                Ok(())
            }
            Value::Array(a) => a
                .iter()
                .filter(|x| x.is_string())
                .try_for_each(|x| self.check_bind_ids(x)),
            _ => Ok(()),
        }
    }

    /// Query and bind vars for an AQL query over project data.
    fn aql_prepare(&self, query: &str, bind: &Value) -> Result<(String, Value)> {
        let mut b = match bind {
            Value::Object(m) => m.clone(),
            Value::Null => serde_json::Map::new(),
            other => {
                return Err(StoreError::Invalid(format!(
                    "bind vars must be an object, got {other}"
                )))
            }
        };
        for name in collection_bind_names(query) {
            let key = format!("@{name}");
            if !b.contains_key(&key) && (is_project_collection(&name) || valid_collection(&name)) {
                b.insert(key, json!(name));
            }
        }
        let coll_of = |bind: &str| -> Option<String> {
            b.get(&format!("@{bind}"))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let q = inject_project_filters(query, &coll_of);
        if q.contains("LENGTH(@@") || q.contains("COUNT(@@") {
            for n in collection_bind_names(query) {
                if coll_of(&n).is_some_and(|c| is_project_collection(&c))
                    && (q.contains(&format!("LENGTH(@@{n})"))
                        || q.contains(&format!("COUNT(@@{n})")))
                {
                    return Err(StoreError::Invalid(format!(
                        "LENGTH(@@{n}) counts every project; count a filtered scan instead"
                    )));
                }
            }
        }
        for (k, v) in b.iter() {
            if !k.starts_with('@') {
                self.check_bind_ids(v)?;
            }
        }
        if uses_param(&q, "kodehub_graph") && !b.contains_key("kodehub_graph") {
            b.insert("kodehub_graph".into(), json!(GRAPH_NAME));
        }
        if uses_param(&q, PROJECT_BIND) {
            b.insert(PROJECT_BIND.into(), json!(self.require_project()?));
        }
        Ok((q, Value::Object(b)))
    }

    fn db_url(&self, path: &str) -> String {
        format!("{}/_db/{}{}", self.cfg.url, self.cfg.database, path)
    }

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

    /// Database-level call (schema and admin endpoints, global collections).
    pub async fn db_raw(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        self.send_raw(method, self.db_url(path), body, None).await
    }

    /// AQL with no project handling (global collections and explicitly cross-project admin
    /// queries, which must filter by `project` themselves).
    pub async fn aql_raw(&self, query: &str, bind: Value) -> Result<Vec<Value>> {
        self.cursor(query, bind, None).await
    }

    async fn cursor(&self, query: &str, bind: Value, trx: Option<&str>) -> Result<Vec<Value>> {
        let body = json!({"query": query, "bindVars": bind, "batchSize": 1000});
        let mut v = self
            .send_raw(Method::POST, self.db_url("/_api/cursor"), Some(&body), trx)
            .await?;
        let mut out: Vec<Value> = v["result"].as_array().cloned().unwrap_or_default();
        while v["hasMore"].as_bool() == Some(true) {
            let id = v["id"].as_str().unwrap_or_default().to_string();
            v = self
                .send_raw(
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

    pub async fn version(&self) -> Result<Value> {
        self.send_raw(
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
            .send_raw(
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
        match self.send_raw(Method::DELETE, url, None, None).await {
            Ok(_) => Ok(()),
            Err(StoreError::Arango { num: 1228, .. }) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Run AQL over project data (see the module docs), following cursors until exhausted.
    pub async fn aql(&self, query: &str, bind: Value, trx: Option<&str>) -> Result<Vec<Value>> {
        let (q, b) = self.aql_prepare(query, &bind)?;
        self.cursor(&q, b, trx).await
    }

    pub async fn begin(&self, write: &[&str], read: &[&str]) -> Result<String> {
        let body = json!({"collections": {"write": write, "read": read}, "waitForSync": false});
        let v = self
            .send_raw(
                Method::POST,
                self.db_url("/_api/transaction/begin"),
                Some(&body),
                None,
            )
            .await?;
        Ok(v["result"]["id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn commit(&self, trx: &str) -> Result<()> {
        self.send_raw(
            Method::PUT,
            self.db_url(&format!("/_api/transaction/{trx}")),
            None,
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn abort(&self, trx: &str) -> Result<()> {
        self.send_raw(
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
        let doc = self.prepare(coll, doc)?;
        self.send_raw(
            Method::POST,
            self.db_url(&format!("/_api/document/{coll}?returnNew=true")),
            Some(&doc),
            trx,
        )
        .await
        .map(|v| v["new"].clone())
    }

    /// Insert, ignoring an existing document with the same key.
    pub async fn insert_ignore(&self, coll: &str, doc: &Value, trx: Option<&str>) -> Result<()> {
        check(coll, "x")?;
        let doc = self.prepare(coll, doc)?;
        self.send_raw(
            Method::POST,
            self.db_url(&format!("/_api/document/{coll}?overwriteMode=ignore")),
            Some(&doc),
            trx,
        )
        .await
        .map(|_| ())
    }

    /// Insert many documents (each prepared like `insert`), replacing existing keys.
    pub async fn insert_many_replace(&self, coll: &str, docs: &[Value]) -> Result<()> {
        check(coll, "x")?;
        let docs: Vec<Value> = docs
            .iter()
            .map(|d| self.prepare(coll, d))
            .collect::<Result<_>>()?;
        for chunk in docs.chunks(500) {
            let v = self
                .send_raw(
                    Method::POST,
                    self.db_url(&format!("/_api/document/{coll}?overwriteMode=replace")),
                    Some(&Value::Array(chunk.to_vec())),
                    None,
                )
                .await?;
            if let Some(err) = v
                .as_array()
                .and_then(|a| a.iter().find(|r| r["error"] == true))
            {
                return Err(StoreError::Arango {
                    code: 400,
                    num: err["errorNum"].as_i64().unwrap_or(0),
                    message: err["errorMessage"].as_str().unwrap_or("").to_string(),
                });
            }
        }
        Ok(())
    }

    pub async fn get(&self, coll: &str, key: &str, trx: Option<&str>) -> Result<Value> {
        check(coll, key)?;
        let key = &self.lookup_key(coll, key)?;
        if !self.own_key(coll, key)? {
            return Err(StoreError::NotFound(format!("{coll}/{key}")));
        }
        self.send_raw(
            Method::GET,
            self.db_url(&format!("/_api/document/{coll}/{key}")),
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
        let key = &self.lookup_key(coll, key)?;
        if !self.own_key(coll, key)? {
            return Err(StoreError::NotFound(format!("{coll}/{key}")));
        }
        let mut doc = doc.clone();
        if let Some(m) = doc.as_object_mut() {
            m.remove("project");
            m.remove("_key");
            for end in ["_from", "_to"] {
                if let Some(id) = m.get(end).and_then(Value::as_str) {
                    let id = id.to_string();
                    self.check_id(&id, end)?;
                }
            }
        }
        self.send_raw(
            Method::PATCH,
            self.db_url(&format!(
                "/_api/document/{coll}/{key}?returnNew=true&mergeObjects=false"
            )),
            Some(&doc),
            trx,
        )
        .await
        .map(|v| v["new"].clone())
        .map_err(|e| match e {
            StoreError::Arango { num: 1202, .. } => StoreError::NotFound(format!("{coll}/{key}")),
            other => other,
        })
    }

    pub async fn remove(&self, coll: &str, key: &str, trx: Option<&str>) -> Result<()> {
        check(coll, key)?;
        let key = &self.lookup_key(coll, key)?;
        if !self.own_key(coll, key)? {
            return Err(StoreError::NotFound(format!("{coll}/{key}")));
        }
        self.send_raw(
            Method::DELETE,
            self.db_url(&format!("/_api/document/{coll}/{key}")),
            None,
            trx,
        )
        .await
        .map(|_| ())
    }

    /// Insert or replace by `_key`.
    pub async fn upsert(&self, coll: &str, doc: &Value, trx: Option<&str>) -> Result<()> {
        check(coll, "x")?;
        let doc = self.prepare(coll, doc)?;
        self.send_raw(
            Method::POST,
            self.db_url(&format!("/_api/document/{coll}?overwriteMode=replace")),
            Some(&doc),
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
        assert!(valid_key("shop.rust-hash_password"));
        assert!(!valid_key("../versions/abc"));
        assert!(!valid_key("a/b"));
        assert!(!valid_key("a%2Fb"));
        assert!(!valid_key("a?x=1"));
        assert!(!valid_key(".."));
        assert!(!valid_collection("versions/x"));
    }

    #[test]
    fn qualified_keys() {
        assert_eq!(qualify("shop", "rust-x"), "shop.rust-x");
        assert_eq!(qualify("shop", "shop.rust-x"), "shop.rust-x");
        assert_eq!(qualify("shop", "shopx.y"), "shop.shopx.y");
        assert_eq!(key_project("shop.rust-x"), Some("shop"));
        assert_eq!(key_project("rust-x"), None);
        assert_eq!(key_project("Shop.x"), None);
        assert_eq!(local_key("shop.a.b"), "a.b");
        assert_eq!(local_key("plain"), "plain");
    }

    #[test]
    fn scans_get_a_project_filter() {
        let coll_of = |b: &str| -> Option<String> {
            Some(match b {
                "c" => "work_items".to_string(),
                other => other.to_string(),
            })
        };
        let q = inject_project_filters(
            "FOR s IN @@code FILTER s.x == 1 FOR e IN @@has_version RETURN e",
            &coll_of,
        );
        assert_eq!(
            q,
            "FOR s IN @@code FILTER s.project == @kh_project FILTER s.x == 1 \
             FOR e IN @@has_version FILTER e.project == @kh_project RETURN e"
        );
        // a global collection bound through @@c is not filtered
        let q = inject_project_filters("FOR w IN @@c RETURN w", &coll_of);
        assert_eq!(q, "FOR w IN @@c RETURN w");
        // traversals and words containing FOR are untouched
        let q = inject_project_filters(
            "LET platform = 1 FOR v IN 1..3 OUTBOUND @n @@contains RETURN v",
            &coll_of,
        );
        assert_eq!(
            q,
            "LET platform = 1 FOR v IN 1..3 OUTBOUND @n @@contains RETURN v"
        );
        // multi-byte text around the clause is kept intact
        let q = inject_project_filters("RETURN 'a — b' FOR x IN @@code RETURN x", &coll_of);
        assert_eq!(
            q,
            "RETURN 'a — b' FOR x IN @@code FILTER x.project == @kh_project RETURN x"
        );
        // case-insensitive, multi-line
        let q = inject_project_filters("for x\n  in   @@requirements return x", &coll_of);
        assert_eq!(
            q,
            "for x\n  in   @@requirements FILTER x.project == @kh_project return x"
        );
    }
}
