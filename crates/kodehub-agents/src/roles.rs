//! Agent types (spec §4.1): one base agent instantiated per work item with a role, instructions,
//! tools and context. Built-in types are seeded into each project's `agent_types` collection;
//! projects can add custom types (`custom:<name>`, e.g. a policy agent) with their own role text.
//!
//! Every non-coding role answers with the same structured shape — a change log and a list of
//! actions the orchestrator applies through the graph API (`actions.rs`). Coding work goes
//! through the changeset engine (plan → tests → code → blind review → formal tests → promote).

use kodehub_store::Store;
use serde_json::{json, Value};

use crate::{Result, Role};

/// (agent type, prompt file, tool role, description)
pub const BUILTIN: &[(&str, &str, Role, &str)] = &[
    (
        "coding",
        "coder",
        Role::Coder,
        "Changes code at the code-node level through changesets.",
    ),
    (
        "test_author",
        "test_author",
        Role::Tester,
        "Writes deterministic shell-run tests and owns the test schedule.",
    ),
    (
        "review",
        "reviewer",
        Role::Reviewer,
        "Blind review of a changeset.",
    ),
    (
        "planning",
        "planning",
        Role::Orchestrator,
        "Breaks direction into containers, components and work items.",
    ),
    (
        "architect",
        "architect",
        Role::Orchestrator,
        "Higher-level decisions: hard failures, conflicts, deadlocks, use cases.",
    ),
    (
        "import",
        "import",
        Role::Orchestrator,
        "Maps an imported repository onto the C4 graph.",
    ),
    (
        "design",
        "design",
        Role::Orchestrator,
        "UI designs that satisfy use cases, handed to coding agents.",
    ),
    (
        "sysadmin",
        "sysadmin",
        Role::Orchestrator,
        "Sets agent caps to fit the cluster.",
    ),
];

pub fn prompt_text(file: &str) -> &'static str {
    match file {
        "coder" => include_str!("../prompts/coder.md"),
        "reviewer" => include_str!("../prompts/reviewer.md"),
        "tester" => include_str!("../prompts/tester.md"),
        "test_author" => include_str!("../prompts/test_author.md"),
        "planning" => include_str!("../prompts/planning.md"),
        "architect" => include_str!("../prompts/architect.md"),
        "import" => include_str!("../prompts/import.md"),
        "design" => include_str!("../prompts/design.md"),
        "sysadmin" => include_str!("../prompts/sysadmin.md"),
        _ => include_str!("../prompts/custom.md"),
    }
}

const CONVENTIONS: &str = include_str!("../prompts/conventions.md");

/// Seed the built-in agent types into the project (idempotent).
pub async fn ensure_agent_types(store: &Store) -> Result<()> {
    for (t, file, role, desc) in BUILTIN {
        store
            .db
            .insert_ignore(
                "agent_types",
                &json!({"_key": t, "builtin": true, "prompt": file, "tool_role": role,
                        "description": desc, "model": null}),
                None,
            )
            .await?;
    }
    Ok(())
}

/// The agent type document (`custom:<name>` maps to key `custom-<name>`).
pub async fn agent_type(store: &Store, t: &str) -> Result<Value> {
    ensure_agent_types(store).await?;
    let key = t.replace(':', "-");
    Ok(store.db.get("agent_types", &key, None).await?)
}

/// System prompt for an agent type.
pub fn system_for(doc: &Value) -> String {
    let file = doc["prompt"].as_str().unwrap_or("custom");
    let base = prompt_text(file);
    base.replace("{{ROLE}}", doc["instructions"].as_str().unwrap_or(""))
        .replace("{{CONVENTIONS}}", CONVENTIONS)
}

pub fn tool_role(doc: &Value) -> Role {
    serde_json::from_value(doc["tool_role"].clone()).unwrap_or(Role::Orchestrator)
}

/// The answer every non-coding role gives.
pub fn actions_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "change_log": {"type": "string", "description": "What you decided and why, for future agents and humans."},
            "actions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "type": {"type": "string", "enum": [
                            "create_nodes", "enqueue", "propose_requirement_change", "set_philosophy",
                            "write_tests", "schedule", "set_caps", "external_blocker",
                            "use_case_verdict", "import_mapping", "note"
                        ]}
                    },
                    "required": ["type"],
                    "additionalProperties": true
                }
            }
        },
        "required": ["change_log", "actions"],
        "additionalProperties": false
    })
}
