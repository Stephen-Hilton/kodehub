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
    TestedWith,
    BindsTo,
}

impl CodeEdgeKind {
    pub fn collection(&self) -> &'static str {
        match self {
            CodeEdgeKind::Calls => "calls",
            CodeEdgeKind::DependsOn => "depends_on",
            CodeEdgeKind::MemberOf => "member_of",
            CodeEdgeKind::TestedWith => "tested_with",
            CodeEdgeKind::BindsTo => "binds_to",
        }
    }
    pub fn from_collection(s: &str) -> Option<Self> {
        Some(match s {
            "calls" => CodeEdgeKind::Calls,
            "depends_on" => CodeEdgeKind::DependsOn,
            "member_of" => CodeEdgeKind::MemberOf,
            "tested_with" => CodeEdgeKind::TestedWith,
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

/// Project-scoped document collections (docs/ontology.md), in bootstrap order. Every document
/// carries `project` and a `<project>.`-qualified key (D-055).
pub const DOCUMENT_COLLECTIONS: &[&str] = &[
    "philosophy",
    "requirements",
    "requirement_versions",
    "use_cases",
    "agents",
    "agent_chats",
    "agent_chat_msgs",
    "container_images",
    "schedules",
    "contexts",
    "containers",
    "components",
    "code",
    "tests",
    "project_versions",
    "context_versions",
    "container_versions",
    "component_versions",
    "code_versions",
    "test_versions",
    "builds",
    "test_logs",
    "environments",
    "deploy_targets",
    "deployers",
    "deployments",
    "deploy_logs",
    // system collection (not graph data): a Git import's staged units
    "import_staging",
];

/// Project-scoped edge collections (docs/ontology.md).
pub const EDGE_COLLECTIONS: &[&str] = &[
    "owns",
    "contains",
    "has_version",
    "based_on",
    "calls",
    "depends_on",
    "member_of",
    "binds_to",
    "tested_with",
    "applies_to",
    "verified_by",
    "relates_to",
    "has_message",
    "authored",
    "refers_to",
    "has_work_item",
    "intends_changes",
    "targets",
    "build_of",
    "built_for",
    "logs_to",
    "deployed_to",
    "has_deployment",
    "uses",
    "has_deployer",
    "runs",
];

const C4: &[&str] = &["contexts", "containers", "components"];
const LEAVES: &[&str] = &["code", "tests"];
const NODES: &[&str] = &["contexts", "containers", "components", "code", "tests"];
const VERSIONS: &[&str] = &[
    "project_versions",
    "context_versions",
    "container_versions",
    "component_versions",
    "code_versions",
    "test_versions",
];

/// The named graph's edge definitions: which collections each edge may connect (from, to).
/// The database refuses an edge between any other pair. `projects`, `users`, `orgs`,
/// `remote_agents` and `work_items` are global collections that project edges may touch.
pub fn edge_definitions() -> Vec<(&'static str, Vec<&'static str>, Vec<&'static str>)> {
    let cat = |a: &[&[&'static str]]| -> Vec<&'static str> {
        let mut v: Vec<&'static str> = a.iter().flat_map(|x| x.iter().copied()).collect();
        v.sort();
        v.dedup();
        v
    };
    let all_nodes: Vec<&'static str> = DOCUMENT_COLLECTIONS
        .iter()
        .copied()
        .filter(|c| *c != "import_staging")
        .chain(["projects", "work_items"])
        .collect();
    vec![
        (
            "owns",
            vec!["projects"],
            vec![
                "philosophy",
                "requirements",
                "use_cases",
                "agents",
                "agent_chats",
                "container_images",
                "schedules",
                "environments",
                "deploy_targets",
            ],
        ),
        (
            "contains",
            cat(&[&["projects"], C4, VERSIONS]),
            cat(&[NODES, VERSIONS]),
        ),
        (
            "has_version",
            cat(&[&["projects", "requirements"], NODES]),
            cat(&[VERSIONS, &["requirement_versions"]]),
        ),
        (
            "based_on",
            vec!["project_versions"],
            vec!["project_versions"],
        ),
        ("calls", LEAVES.to_vec(), vec!["code"]),
        ("depends_on", LEAVES.to_vec(), vec!["code"]),
        ("member_of", vec!["code"], vec!["code"]),
        ("binds_to", vec!["code"], vec!["code"]),
        ("tested_with", cat(&[&["code"], C4]), vec!["tests"]),
        (
            "applies_to",
            vec!["requirements"],
            cat(&[&["projects"], C4, &["code"]]),
        ),
        (
            "verified_by",
            vec!["requirements", "use_cases"],
            vec!["tests"],
        ),
        ("relates_to", vec!["use_cases"], vec!["requirements"]),
        ("has_message", vec!["agent_chats"], vec!["agent_chat_msgs"]),
        (
            "authored",
            vec!["agents", "users", "remote_agents"],
            vec!["agent_chat_msgs"],
        ),
        ("refers_to", vec!["agent_chat_msgs"], all_nodes.clone()),
        (
            "has_work_item",
            vec!["project_versions"],
            vec!["work_items"],
        ),
        ("intends_changes", vec!["work_items"], NODES.to_vec()),
        ("targets", vec!["schedules"], cat(&[&["projects"], NODES])),
        (
            "build_of",
            vec!["builds"],
            vec![
                "code_versions",
                "component_versions",
                "container_versions",
                "test_versions",
            ],
        ),
        ("built_for", vec!["builds"], vec!["deploy_targets"]),
        (
            "logs_to",
            vec!["test_versions", "code_versions", "builds", "deployments"],
            vec!["test_logs", "deploy_logs"],
        ),
        (
            "deployed_to",
            vec![
                "context_versions",
                "container_versions",
                "component_versions",
                "code_versions",
            ],
            vec!["deploy_targets"],
        ),
        (
            "has_deployment",
            vec!["project_versions", "environments"],
            vec!["deployments"],
        ),
        ("uses", vec!["deployments"], vec!["deployers"]),
        (
            "has_deployer",
            vec!["environments", "deploy_targets"],
            vec!["deployers"],
        ),
        ("runs", vec!["deployers"], vec!["container_images"]),
    ]
}

/// Collections whose documents may never be updated after creation.
pub const IMMUTABLE_COLLECTIONS: &[&str] = &[
    "context_versions",
    "container_versions",
    "component_versions",
    "code_versions",
    "test_versions",
    "requirement_versions",
    "builds",
    "test_logs",
    "deploy_logs",
    "agent_chat_msgs",
];

/// Default environments, lowest first (the `environments` setting may change them).
pub const ENVIRONMENTS: &[&str] = &["dev", "test", "qa", "preprod", "prod"];

/// Stages of a work item's proposal (D-058; the v0.1 changeset states).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    Draft,
    Planned,
    Approved,
    Building,
    Reviewing,
    Testing,
    Ready,
    Accepted,
    Rejected,
    Escalated,
}

impl ProposalState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProposalState::Draft => "draft",
            ProposalState::Planned => "planned",
            ProposalState::Approved => "approved",
            ProposalState::Building => "building",
            ProposalState::Reviewing => "reviewing",
            ProposalState::Testing => "testing",
            ProposalState::Ready => "ready",
            ProposalState::Accepted => "accepted",
            ProposalState::Rejected => "rejected",
            ProposalState::Escalated => "escalated",
        }
    }
    /// Accepted and rejected are final. Escalated waits for a human resolution (review M6 #9).
    pub fn is_terminal(&self) -> bool {
        matches!(self, ProposalState::Accepted | ProposalState::Rejected)
    }
    /// Legal forward transitions. Rework loops go back to `Building`.
    pub fn can_transition_to(&self, next: ProposalState) -> bool {
        use ProposalState::*;
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
                | (Ready, Accepted)
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
    fn every_edge_collection_has_a_definition() {
        let defs = edge_definitions();
        for e in EDGE_COLLECTIONS {
            assert!(defs.iter().any(|(c, _, _)| c == e), "{e}");
        }
        for (c, from, to) in &defs {
            assert!(EDGE_COLLECTIONS.contains(c), "{c}");
            for x in from.iter().chain(to) {
                assert!(
                    DOCUMENT_COLLECTIONS.contains(x)
                        || ["projects", "work_items", "users", "remote_agents"].contains(x),
                    "{c}: {x}"
                );
            }
        }
    }

    #[test]
    fn proposal_transitions() {
        use ProposalState::*;
        assert!(Draft.can_transition_to(Planned));
        assert!(!Draft.can_transition_to(Accepted));
        assert!(Testing.can_transition_to(Building));
        assert!(Building.can_transition_to(Escalated));
        assert!(!Accepted.can_transition_to(Rejected));
        assert!(Escalated.can_transition_to(Building));
        assert!(Ready.can_transition_to(Testing));
    }
}
