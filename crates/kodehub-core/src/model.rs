//! Model types shared by every crate. Field names match the ArangoDB documents (doc 02).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Function,
    Class,
    Trait,
    Enum,
    Test,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Class => "class",
            Kind::Trait => "trait",
            Kind::Enum => "enum",
            Kind::Test => "test",
        }
    }
    /// Types are rendered before functions when ordering is otherwise free.
    pub fn is_type(&self) -> bool {
        matches!(self, Kind::Class | Kind::Trait | Kind::Enum)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    Python,
}

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    Public,
    Internal,
}

/// The hashed part of a version. Display names are deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionContent {
    pub kind: Kind,
    pub language: Language,
    /// Name-free signature, e.g. `(password: &str) -> String`.
    pub signature: String,
    /// Item body without the enclosing braces / `def` line, unindented.
    pub body: String,
    #[serde(default = "empty_object")]
    pub attributes: Value,
}

pub fn empty_object() -> Value {
    Value::Object(Default::default())
}

impl VersionContent {
    pub fn hash(&self) -> String {
        crate::canonical::content_hash(self)
    }
    /// A copy with signature/body normalised exactly as hashed.
    pub fn normalized(&self) -> VersionContent {
        VersionContent {
            kind: self.kind,
            language: self.language,
            signature: crate::canonical::normalize_text(&self.signature)
                .trim()
                .to_string(),
            body: crate::canonical::normalize_text(&self.body),
            attributes: self.attributes.clone(),
        }
    }
}

/// Edge collections between code symbols that matter for rendering and closure hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeEdgeKind {
    Calls,
    DependsOn,
    MemberOf,
    TestedBy,
    BindsTo,
}

impl CodeEdgeKind {
    pub fn collection(&self) -> &'static str {
        match self {
            CodeEdgeKind::Calls => "calls",
            CodeEdgeKind::DependsOn => "depends_on",
            CodeEdgeKind::MemberOf => "member_of",
            CodeEdgeKind::TestedBy => "tested_by",
            CodeEdgeKind::BindsTo => "binds_to",
        }
    }
    pub fn from_collection(s: &str) -> Option<Self> {
        Some(match s {
            "calls" => CodeEdgeKind::Calls,
            "depends_on" => CodeEdgeKind::DependsOn,
            "member_of" => CodeEdgeKind::MemberOf,
            "tested_by" => CodeEdgeKind::TestedBy,
            "binds_to" => CodeEdgeKind::BindsTo,
            _ => return None,
        })
    }
    /// Edges that make the target part of the source's build closure.
    pub fn is_build_dependency(&self) -> bool {
        matches!(
            self,
            CodeEdgeKind::Calls
                | CodeEdgeKind::DependsOn
                | CodeEdgeKind::MemberOf
                | CodeEdgeKind::BindsTo
        )
    }
}

/// Requirement example / edge case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Example {
    pub description: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub expected: Value,
}

/// Health / coverage status used everywhere (map colours, badges, merge checks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Pass,
    AtRisk,
    Fail,
    Uncovered,
    Orphan,
}

/// All document collections (doc 02), in bootstrap order.
pub const DOCUMENT_COLLECTIONS: &[&str] = &[
    "missions",
    "requirements",
    "requirement_groups",
    "requirement_history",
    "decisions",
    "contexts",
    "containers",
    "components",
    "symbols",
    "versions",
    "build_targets",
    "artifacts",
    "namespaces",
    "changesets",
    "test_runs",
    "runs",
    "releases",
    "environments",
    "chat_messages",
    "philosophy",
    "use_cases",
    "c4_versions",
    "change_logs",
    "intents",
    "negotiations",
    "test_container_types",
];

/// All edge collections (doc 02).
pub const EDGE_COLLECTIONS: &[&str] = &[
    "contains",
    "member_of",
    "has_version",
    "calls",
    "depends_on",
    "tested_by",
    "verifies",
    "satisfies",
    "justified_by",
    "includes",
    "traces_to",
    "binds_to",
    "compiles_to",
    "exposes",
    "proposes",
    "supports",
    "in_environment",
    "cloned_from",
    "relates_to",
    "expected_by",
    "built_from",
];

/// Collections whose documents may never be updated after creation.
pub const IMMUTABLE_COLLECTIONS: &[&str] =
    &["versions", "c4_versions", "releases", "test_runs", "runs"];

/// Default environments, lowest first (the `environments` setting may change them).
pub const ENVIRONMENTS: &[&str] = &["dev", "test", "qa", "preprod", "prod"];

/// Changeset states (doc 04).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangesetState {
    Draft,
    Planned,
    Approved,
    Building,
    Reviewing,
    Testing,
    Ready,
    Merged,
    Rejected,
    Escalated,
}

impl ChangesetState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangesetState::Draft => "draft",
            ChangesetState::Planned => "planned",
            ChangesetState::Approved => "approved",
            ChangesetState::Building => "building",
            ChangesetState::Reviewing => "reviewing",
            ChangesetState::Testing => "testing",
            ChangesetState::Ready => "ready",
            ChangesetState::Merged => "merged",
            ChangesetState::Rejected => "rejected",
            ChangesetState::Escalated => "escalated",
        }
    }
    /// Merged and rejected are final. Escalated waits for a human resolution (review M6 #9).
    pub fn is_terminal(&self) -> bool {
        matches!(self, ChangesetState::Merged | ChangesetState::Rejected)
    }
    /// Legal forward transitions. Rework loops go back to `Building`.
    pub fn can_transition_to(&self, next: ChangesetState) -> bool {
        use ChangesetState::*;
        if next == Rejected {
            return !self.is_terminal();
        }
        if next == Escalated {
            return !self.is_terminal() && *self != Escalated;
        }
        matches!(
            (self, next),
            (Draft, Planned)
                | (Planned, Planned)
                | (Planned, Approved)
                | (Approved, Building)
                | (Building, Reviewing)
                | (Reviewing, Testing)
                | (Reviewing, Building)
                | (Testing, Building)
                | (Testing, Ready)
                | (Ready, Merged)
                | (Ready, Building)
                | (Ready, Testing)
                | (Building, Planned)
                | (Escalated, Building)
                | (Escalated, Testing)
        )
    }
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changeset_transitions() {
        use ChangesetState::*;
        assert!(Draft.can_transition_to(Planned));
        assert!(!Draft.can_transition_to(Merged));
        assert!(Testing.can_transition_to(Building));
        assert!(Building.can_transition_to(Escalated));
        assert!(!Merged.can_transition_to(Rejected));
        assert!(Escalated.can_transition_to(Building));
        assert!(Ready.can_transition_to(Testing));
    }
}
