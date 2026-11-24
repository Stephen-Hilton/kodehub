//! Schema bootstrap. `bootstrap_registry` creates the database and the global project registry;
//! `bootstrap` creates the *active project's* collections, edge collections, named graph,
//! indexes and the four environments. Both idempotent.

use kodehub_core::model::{DOCUMENT_COLLECTIONS, EDGE_COLLECTIONS, ENVIRONMENTS};
use reqwest::Method;
use serde_json::json;

use crate::{Arango, Result, StoreError};

pub use crate::arango::GRAPH_NAME;

/// Global (not project-scoped) collections: the project registry and server metadata.
pub const PROJECTS: &str = "kodehub_projects";
pub const META: &str = "kodehub_meta";

/// (collection, fields, unique, sparse)
const INDEXES: &[(&str, &[&str], bool, bool)] = &[
    // Invariant 1: a node has at most one stable version (`stable_of` is set only on the stable
    // edge; the sparse unique index ignores every other edge).
    ("has_version", &["stable_of"], true, true),
    ("has_version", &["_from", "seq"], false, false),
    ("has_version", &["changeset"], false, true),
    // Invariant 5: a release is in at most one environment (`current_release` is set only on
    // the open edge), and a single-slot environment holds one release (`slot`).
    ("in_environment", &["current_release"], true, true),
    ("in_environment", &["slot"], true, true),
    ("in_environment", &["_to", "validTo"], false, false),
    ("supports", &["_to", "node"], true, false),
    // Display names are unique per language within a project (pilot simplification).
    ("symbols", &["language", "display_name"], true, false),
    ("test_runs", &["test_version", "closure_hash"], false, false),
    ("artifacts", &["closure_hash", "build_target"], true, false),
    ("runs", &["symbol", "ran_at"], false, false),
    (
        "requirement_history",
        &["requirement", "revision"],
        true,
        false,
    ),
    ("changesets", &["state"], false, false),
    ("chat_messages", &["created_at"], false, false),
];

async fn create_collection(db: &Arango, name: &str, edge: bool) -> Result<()> {
    let body = json!({"name": name, "type": if edge { 3 } else { 2 }});
    match db
        .db_raw(Method::POST, "/_api/collection", Some(&body))
        .await
    {
        Ok(_) | Err(StoreError::Arango { num: 1207, .. }) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Global (cross-project) document collections beyond the registry.
pub fn global_document_collections() -> Vec<&'static str> {
    let mut v = vec![
        PROJECTS,
        META,
        crate::settings::SETTINGS,
        crate::credentials::CREDENTIALS,
        crate::audit::AUDIT,
        crate::queue::WORK_ITEMS,
        crate::schedules::SCHEDULES,
        crate::schedules::TRIGGERS,
        crate::metering::USAGE,
        crate::governance::NOTIFICATIONS,
        crate::oauth::CLIENTS,
        crate::oauth::CODES,
    ];
    v.extend(crate::identity::IDENTITY_DOCS);
    v
}

/// (collection, fields, unique, sparse) for global collections.
const GLOBAL_INDEXES: &[(&str, &[&str], bool, bool)] = &[
    ("kodehub_settings", &["scope", "key"], true, false),
    ("kodehub_audit", &["project", "at"], false, false),
    ("kodehub_audit", &["actor", "at"], false, false),
    ("kodehub_member_of", &["_from", "_to"], true, false),
    ("kodehub_acts_for", &["_from"], true, false),
    ("kodehub_tokens", &["user"], false, false),
    ("kodehub_work_items", &["state", "priority"], false, false),
    ("kodehub_work_items", &["project", "state"], false, false),
    ("kodehub_schedules", &["enabled", "next_due"], false, false),
    ("kodehub_schedules", &["project"], false, false),
    ("kodehub_triggers", &["state"], false, false),
    ("kodehub_usage", &["project"], false, false),
];

/// Create the database and the global collections (registry, identity, settings, audit).
pub async fn bootstrap_registry(db: &Arango) -> Result<()> {
    db.ensure_database().await?;
    for c in global_document_collections() {
        create_collection(db, c, false).await?;
    }
    for c in crate::identity::IDENTITY_EDGES {
        create_collection(db, c, true).await?;
    }
    for (coll, fields, unique, sparse) in GLOBAL_INDEXES {
        let body =
            json!({"type": "persistent", "fields": fields, "unique": unique, "sparse": sparse});
        db.db_raw(
            Method::POST,
            &format!("/_api/index?collection={coll}"),
            Some(&body),
        )
        .await?;
    }
    Ok(())
}

/// Create the active project's schema. Requires a selected project.
pub async fn bootstrap(db: &Arango) -> Result<()> {
    if db.project().is_none() {
        return Err(StoreError::Invalid("no project selected".into()));
    }
    for c in DOCUMENT_COLLECTIONS {
        create_collection(db, &db.phys(c), false).await?;
    }
    for c in EDGE_COLLECTIONS {
        create_collection(db, &db.phys(c), true).await?;
    }
    for (coll, fields, unique, sparse) in INDEXES {
        let body =
            json!({"type": "persistent", "fields": fields, "unique": unique, "sparse": sparse});
        db.db_raw(
            Method::POST,
            &format!("/_api/index?collection={}", db.phys(coll)),
            Some(&body),
        )
        .await?;
    }
    let docs: Vec<String> = DOCUMENT_COLLECTIONS.iter().map(|c| db.phys(c)).collect();
    let defs: Vec<_> = EDGE_COLLECTIONS
        .iter()
        .map(|e| json!({"collection": db.phys(e), "from": docs, "to": docs}))
        .collect();
    match db
        .db_raw(
            Method::POST,
            "/_api/gharial",
            Some(&json!({"name": db.graph(), "edgeDefinitions": defs})),
        )
        .await
    {
        Ok(_) | Err(StoreError::Arango { num: 1925, .. }) => {}
        Err(e) => return Err(e),
    }
    for (i, env) in ENVIRONMENTS.iter().enumerate() {
        db.insert_ignore(
            "environments",
            &json!({
                "_key": env,
                "name": env,
                "order": i,
                "requires_human_approval": *env == "prod",
                "config": {"stubs_allowed": *env == "dev"},
            }),
            None,
        )
        .await?;
    }
    Ok(())
}

/// Drop a project's named graph and every one of its collections.
pub async fn drop_project_schema(db: &Arango, project: &str) -> Result<()> {
    let graph = Arango::phys_in(project, GRAPH_NAME);
    match db
        .db_raw(
            Method::DELETE,
            &format!("/_api/gharial/{graph}?dropCollections=false"),
            None,
        )
        .await
    {
        Ok(_) | Err(StoreError::Arango { num: 1924, .. }) => {}
        Err(e) => return Err(e),
    }
    for c in DOCUMENT_COLLECTIONS.iter().chain(EDGE_COLLECTIONS) {
        let name = Arango::phys_in(project, c);
        match db
            .db_raw(Method::DELETE, &format!("/_api/collection/{name}"), None)
            .await
        {
            Ok(_) | Err(StoreError::Arango { num: 1203, .. }) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
