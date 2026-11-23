//! Kodehub core: model types, canonicalisation, hashing, closure computation and invariants.
//!
//! Everything here is pure (no I/O) so that hashing and rendering decisions are deterministic
//! and testable on any machine.

pub mod canonical;
pub mod config;
pub mod graph;
pub mod infer;
pub mod model;
pub mod sig;
pub mod tools;

pub use canonical::{canonical_json, content_hash, normalize_text};
pub use graph::{CodeEdge, CodeGraph, CodeSymbol, GraphError};
pub use model::*;
