//! Schema bootstrap. `bootstrap_registry` creates the database, the global collections and
//! every project-scoped collection with its indexes and the one named graph (D-055);
//! `bootstrap` creates a project's starting data. All idempotent.

use kodehub_core::model::{DOCUMENT_COLLECTIONS, EDGE_COLLECTIONS};
use reqwest::Method;
use serde_json::json;

use crate::{Arango, Result, StoreError};

pub use crate::arango::GRAPH_NAME;

/// Global (not project-scoped) collections: the project registry and server metadata.
pub const PROJECTS: &str = "projects";
pub const META: &str = "meta";

/// (collection, fields, unique, sparse). Every project collection also gets `[project]`.
const INDEXES: &[(&str, &[&str], bool, bool)] = &[
    ("has_version", &["_from", "seq"], false, false),
    (
        "has_version",
        &["project", "work_item", "state"],
        false,
        true,
    ),
    // display names are unique per language within a project, per collection (code and tests
    // share one namespace, checked on create — pilot simplification)
    (
        "code",
        &["project", "language", "display_name"],
        true,
        false,
    ),
    (
        "tests",
        &["project", "language", "display_name"],
        true,
        false,
    ),
    (
        "test_logs",
        &["project", "test_version", "closure_hash"],
        false,
        false,
    ),
    ("test_logs", &["project", "test", "ran_at"], false, false),
    (
        "builds",
        &["project", "closure_hash", "target"],
        true,
        false,
    ),
    (
        "requirement_versions",
        &["project", "requirement", "revision"],
        false,
        false,
    ),
    (
        "deployments",
        &["project", "environment", "state"],
        false,
        false,
    ),
    (
        "agent_chat_msgs",
        &["project", "chat", "created_at"],
        false,
        false,
    ),
    ("schedules", &["enabled", "next_due"], false, false),
    ("import_staging", &["project", "import"], false, false),
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
    ("settings", &["scope", "key"], true, false),
    ("audit_log", &["project", "at"], false, false),
    ("audit_log", &["actor", "at"], false, false),
    ("works_with", &["_from", "_to"], true, false),
    // a remote agent may act for several users and a user may have many agents (D-061)
    ("acts_for", &["_from", "_to"], true, false),
    ("tokens", &["user"], false, false),
    ("work_items", &["state", "priority"], false, false),
    ("work_items", &["project", "state"], false, false),
    ("schedule_triggers", &["state"], false, false),
    ("usage", &["project"], false, false),
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
    create_collection(db, crate::credentials::SECURES, true).await?;
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
    bootstrap_project_collections(db).await?;
    Ok(())
}

/// Create every project-scoped collection, its indexes and the named graph (once per
/// database; idempotent).
pub async fn bootstrap_project_collections(db: &Arango) -> Result<()> {
    for c in DOCUMENT_COLLECTIONS {
        create_collection(db, c, false).await?;
    }
    for c in EDGE_COLLECTIONS {
        create_collection(db, c, true).await?;
    }
    for c in DOCUMENT_COLLECTIONS.iter().chain(EDGE_COLLECTIONS) {
        let body =
            json!({"type": "persistent", "fields": ["project"], "unique": false, "sparse": false});
        db.db_raw(
            Method::POST,
            &format!("/_api/index?collection={c}"),
            Some(&body),
        )
        .await?;
    }
    for (coll, fields, unique, sparse) in INDEXES {
        let body =
            json!({"type": "persistent", "fields": fields, "unique": unique, "sparse": sparse});
        db.db_raw(
            Method::POST,
            &format!("/_api/index?collection={coll}"),
            Some(&body),
        )
        .await?;
    }
    // the ontology's exact edge definitions: the database refuses an edge between the wrong
    // kinds of node (D-055)
    let defs: Vec<_> = kodehub_core::model::edge_definitions()
        .into_iter()
        .map(|(e, from, to)| json!({"collection": e, "from": from, "to": to}))
        .collect();
    match db
        .db_raw(
            Method::POST,
            "/_api/gharial",
            Some(&json!({"name": GRAPH_NAME, "edgeDefinitions": defs})),
        )
        .await
    {
        Ok(_) | Err(StoreError::Arango { num: 1925, .. }) => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Remove every document of a project from the project-scoped collections.
pub async fn drop_project_data(db: &Arango, project: &str) -> Result<()> {
    for c in EDGE_COLLECTIONS.iter().chain(DOCUMENT_COLLECTIONS) {
        db.aql_raw(
            "FOR d IN @@c FILTER d.project == @p REMOVE d IN @@c",
            json!({"@c": c, "p": project}),
        )
        .await?;
    }
    Ok(())
}
