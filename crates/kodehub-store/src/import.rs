//! Git import (spec §13.1): the import container's code units are staged in the project; the
//! import agent maps them onto contexts, containers and components; the mapping becomes an
//! import changeset whose promotion makes the code live versions.
//!
//! Imported code arrives without Kodehub requirements, so every component gets a generated
//! requirement — "preserve the behaviour of the imported code" — that it satisfies. That keeps
//! the code traceable (no orphans) until humans and agents write real requirements.

use std::collections::BTreeMap;

use kodehub_core::model::{now_iso, Kind, Language};
use serde_json::{json, Value};

use crate::changesets::ProposeVersion;
use crate::{Result, Store, StoreError};

fn unit_key(id: &str) -> String {
    blake3::hash(id.as_bytes()).to_hex()[..24].to_string()
}

impl Store {
    /// Stage parsed units of an import (replacing any earlier staging of the same import).
    pub async fn stage_import(&self, import: &str, meta: &Value, units: &[Value]) -> Result<usize> {
        self.aql(
            "FOR u IN @@import_units FILTER u.import == @i REMOVE u IN @@import_units",
            json!({"i": import}),
        )
        .await?;
        for chunk in units.chunks(200) {
            let docs: Vec<Value> = chunk
                .iter()
                .map(|u| {
                    let mut d = u.clone();
                    d["_key"] = json!(unit_key(&format!(
                        "{import}|{}",
                        u["id"].as_str().unwrap_or("")
                    )));
                    d["import"] = json!(import);
                    d["repo_url"] = meta["repo_url"].clone();
                    d["commit"] = meta["commit"].clone();
                    d["state"] = json!("staged");
                    d
                })
                .collect();
            for d in docs {
                self.db.insert("import_units", &d, None).await?;
            }
        }
        Ok(units.len())
    }

    pub async fn import_units(&self, import: &str) -> Result<Vec<Value>> {
        self.aql(
            "FOR u IN @@import_units FILTER u.import == @i SORT u.file, u.name RETURN u",
            json!({"i": import}),
        )
        .await
    }

    /// Promote an import changeset: the code is imported as it was, so there are no Kodehub tests
    /// to run yet and no code review; orphan checks still apply (each component satisfies its
    /// generated requirement).
    pub async fn merge_import(
        &self,
        key: &str,
        by: &str,
    ) -> Result<crate::changesets::MergeChecks> {
        let cs = self.get_changeset(key).await?;
        if cs["import"] != true {
            return Err(StoreError::Invalid("not an import changeset".into()));
        }
        let mut hist = cs["history"].as_array().cloned().unwrap_or_default();
        hist.push(json!({"state": "ready", "at": now_iso(), "by": by, "note": "git import"}));
        self.patch_changeset(key, json!({"state": "ready", "history": hist}))
            .await?;
        self.merge_bypassing_tests(key, by).await
    }
}

/// Apply an import agent's mapping:
/// `{import, contexts: [{name, description}], containers: [{name, parent}],
///   components: [{name, parent, units: [unit id or id prefix]}]}`.
pub async fn apply_mapping(s: &Store, m: &Value, by: &str) -> Result<Value> {
    let import = m["import"]
        .as_str()
        .ok_or_else(|| StoreError::Invalid("the mapping names its import".into()))?;
    let units = s.import_units(import).await?;
    if units.is_empty() {
        return Err(StoreError::Invalid(format!(
            "no units staged for import {import}"
        )));
    }
    let id_of = |level: &str, name: &str| {
        let coll = match level {
            "context" => "contexts",
            "container" => "containers",
            _ => "components",
        };
        format!("{coll}/{}", crate::slug(name))
    };
    let resolve_parent = |p: &Value, default: &str| -> String {
        match p.as_str() {
            Some(x) if x.contains('/') => x.to_string(),
            Some(x) => {
                // a bare name: a container if one is mapped by that name, else a context
                let cont = m["containers"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|c| c["name"] == x));
                id_of(if cont { "container" } else { "context" }, x)
            }
            None => default.to_string(),
        }
    };
    let mut made = Vec::new();
    for c in m["contexts"].as_array().cloned().unwrap_or_default() {
        let mut n = c.clone();
        n["level"] = json!("context");
        n["parent"] = json!(crate::c4::PHILOSOPHY_ID);
        n["planned_by"] = json!(by);
        made.push(s.create_planned_node(&n).await?);
    }
    let first_ctx = made
        .first()
        .cloned()
        .unwrap_or_else(|| crate::c4::PHILOSOPHY_ID.to_string());
    for c in m["containers"].as_array().cloned().unwrap_or_default() {
        let mut n = c.clone();
        n["level"] = json!("container");
        n["parent"] = json!(resolve_parent(&c["parent"], &first_ctx));
        made.push(s.create_planned_node(&n).await?);
    }
    // units → component
    let mut assigned: BTreeMap<String, String> = BTreeMap::new();
    let mut components = Vec::new();
    for c in m["components"].as_array().cloned().unwrap_or_default() {
        let name = c["name"].as_str().unwrap_or("imported").to_string();
        let mut n = c.clone();
        n["level"] = json!("component");
        n["parent"] = json!(resolve_parent(&c["parent"], &first_ctx));
        let cid = s.create_planned_node(&n).await?;
        components.push((cid.clone(), name.clone()));
        for pat in c["units"].as_array().cloned().unwrap_or_default() {
            let pat = pat.as_str().unwrap_or("");
            for u in &units {
                let uid = u["id"].as_str().unwrap_or("");
                if !pat.is_empty()
                    && (uid == pat || uid.starts_with(pat) || u["file"].as_str() == Some(pat))
                {
                    assigned
                        .entry(uid.to_string())
                        .or_insert_with(|| cid.clone());
                }
            }
        }
    }
    // anything the agent left out still lands somewhere visible
    let leftovers: Vec<&Value> = units
        .iter()
        .filter(|u| !assigned.contains_key(u["id"].as_str().unwrap_or("")))
        .collect();
    if !leftovers.is_empty() {
        let n = json!({"level": "component", "name": "unmapped imported code", "parent": first_ctx, "planned_by": by});
        let cid = s.create_planned_node(&n).await?;
        components.push((cid.clone(), "unmapped imported code".into()));
        for u in leftovers {
            assigned.insert(u["id"].as_str().unwrap_or("").to_string(), cid.clone());
        }
    }
    // one generated requirement per component keeps the imported code traceable
    let mut reqs = Vec::new();
    for (cid, name) in &components {
        let rk = format!("imported-{}", crate::slug(name));
        if s.db.get("requirements", &rk, None).await.is_err() {
            s.create_requirement(json!({
                "_key": rk, "title": format!("Preserve imported behaviour: {name}"),
                "statement": format!("{name} keeps the behaviour of the code imported from {}.", units[0]["repo_url"].as_str().unwrap_or("Git")),
                "intent": "Generated at import; replace with real requirements.", "kind": "technical",
                "covered_revision": 1,
            }))
            .await?;
        }
        let rid = format!("requirements/{rk}");
        s.db.insert_ignore(
            "satisfies",
            &json!({"_key": crate::edge_key("satisfies", cid, &rid), "_from": cid, "_to": rid}),
            None,
        )
        .await?;
        reqs.push(rk);
    }
    // the import changeset: decomposed Rust / Python units become code versions
    let cs = s
        .open_changeset(
            &format!(
                "Import {}",
                units[0]["repo_url"].as_str().unwrap_or("repository")
            ),
            &format!(
                "Git import {import} at {}",
                units[0]["commit"].as_str().unwrap_or("?")
            ),
            &reqs,
            by,
        )
        .await?;
    let key = cs["_key"].as_str().unwrap_or_default().to_string();
    s.patch_changeset(&key, json!({"import": true, "import_id": import}))
        .await?;
    let mut proposed = 0usize;
    let mut opaque = 0usize;
    for u in &units {
        let uid = u["id"].as_str().unwrap_or("");
        let comp = assigned.get(uid).cloned().unwrap_or_default();
        let lang: Option<Language> = match u["language"].as_str() {
            Some("rust") => Some(Language::Rust),
            Some("python") => Some(Language::Python),
            _ => None,
        };
        let kind = match u["kind"].as_str() {
            Some("function") => Some(Kind::Function),
            Some("class") => Some(Kind::Class),
            Some("enum") => Some(Kind::Enum),
            Some("trait") => Some(Kind::Trait),
            _ => None,
        };
        let (Some(lang), Some(kind), true) = (lang, kind, u["decomposed"] == true) else {
            // other languages: opaque modules, kept staged on their component, not decomposed
            s.db.patch(
                "import_units",
                u["_key"].as_str().unwrap_or_default(),
                &json!({"state": "opaque", "component": comp}),
                None,
            )
            .await?;
            opaque += 1;
            continue;
        };
        s.propose_version(
            &key,
            &ProposeVersion {
                symbol: None,
                display_name: u["name"].as_str().map(str::to_string),
                kind: Some(kind),
                language: Some(lang),
                visibility: None,
                signature: u["signature"].as_str().unwrap_or("").into(),
                body: u["body"].as_str().unwrap_or("").into(),
                attributes: Some(json!({"imported_from": u["file"], "import": import})),
                component: Some(crate::key_of(&comp).to_string()),
                member_of: None,
                verifies: vec![],
                tests_for: vec![],
                description: Some(format!(
                    "Imported from {}",
                    u["file"].as_str().unwrap_or("")
                )),
                by: Some(by.to_string()),
            },
        )
        .await?;
        proposed += 1;
    }
    if proposed > 0 {
        s.merge_import(&key, by).await?;
    }
    s.aql(
        "FOR u IN @@import_units FILTER u.import == @i AND u.state == 'staged'
           UPDATE u WITH {state: 'imported'} IN @@import_units",
        json!({"i": import}),
    )
    .await?;
    Ok(
        json!({"changeset": key, "imported": proposed, "opaque": opaque,
              "components": components.iter().map(|c| c.0.clone()).collect::<Vec<_>>(), "nodes": made}),
    )
}
