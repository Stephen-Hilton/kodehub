//! ArangoDB-backed graph store. The only crate that talks to the database.

pub mod arango;
pub mod changesets;
pub mod projects;
pub mod queries;
pub mod releases;
pub mod schema;
pub mod seed;
pub mod store;
pub mod testutil;

pub use arango::{Arango, ArangoConfig};
pub use store::*;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error {code}/{num}: {message}")]
    Arango {
        code: u16,
        num: i64,
        message: String,
    },
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invariant violated: {0}")]
    Invariant(String),
    #[error("immutable: {0}")]
    Immutable(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error(transparent)]
    Graph(#[from] kodehub_core::GraphError),
    #[error("seed error: {0}")]
    Seed(String),
}

impl StoreError {
    /// ArangoDB unique constraint violation.
    pub fn is_unique_violation(&self) -> bool {
        matches!(self, StoreError::Arango { num: 1210, .. })
    }
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            StoreError::NotFound(_) | StoreError::Arango { num: 1202, .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;
