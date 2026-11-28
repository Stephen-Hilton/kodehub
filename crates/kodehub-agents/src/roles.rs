//! Agent types (spec §4.1, D-062): one base agent instantiated per work item with a role,
//! instructions, tools and context. The types live in each project's `agents` collection
//! (`kodehub_store::agents`): built-ins seeded from the defaults, each with a start-up prompt a
//! project can change and the permissions it holds; projects can add custom types
//! (`custom:<name>`, e.g. a policy agent). The prompt templates live here.
//!
//! Every non-coding role answers with the same structured shape — a change log and a list of
//! actions the orchestrator applies through the graph API (`actions.rs`), each checked against
//! the type's permissions. Coding work goes through the work item's proposal (plan → tests →
//! code → blind review → formal tests → acceptance).

use kodehub_store::Store;
use serde_json::{json, Value};

use crate::{Result, Role};

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
    Ok(store.ensure_agents().await?)
}

/// The agent type document (`custom:<name>` is stored as `custom-<name>`).
pub async fn agent_type(store: &Store, t: &str) -> Result<Value> {
    Ok(store.agent(t).await?)
}

/// System prompt for an agent type: its template, with the project's start-up prompt as the
/// role text of a custom type, or added as project instructions to a built-in one.
pub fn system_for(doc: &Value) -> String {
    let file = doc["prompt_file"]
        .as_str()
        .or(doc["prompt"].as_str().filter(|_| doc["builtin"] == true))
        .unwrap_or("custom");
    let base = prompt_text(file);
    let custom = doc["prompt"]
        .as_str()
        .filter(|p| !p.trim().is_empty() && doc["prompt_file"].is_string())
        .or(doc["instructions"].as_str())
        .unwrap_or("");
    let mut out = base
        .replace("{{ROLE}}", custom)
        .replace("{{CONVENTIONS}}", CONVENTIONS);
    if doc["builtin"] == true && !custom.trim().is_empty() && !base.contains("{{ROLE}}") {
        out.push_str(&format!("\n\n## Project instructions\n{custom}\n"));
    }
    out
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
