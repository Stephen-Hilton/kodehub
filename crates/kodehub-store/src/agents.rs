//! Internal agent types per project (D-062, docs/ontology.md `agents`).
//!
//! Every project starts with the built-in types. A project admin can change a type's start-up
//! prompt and what it may do — its **permissions**, one per discrete action — or add custom
//! types. Defaults are wide: one agent doing most of a build cycle is the cheapest (fewer work
//! items, shorter builds, fewer tokens); review and testing stay separate for safety, so a
//! builder never checks its own work. Whatever the permissions say, the reviewer of a work item
//! is never the agent that wrote it (enforced in `proposals.rs`).

use kodehub_core::model::now_iso;
use serde::Serialize;
use serde_json::{json, Value};

use crate::{Result, Store, StoreError};

/// Every permission an internal agent type can hold.
pub const PERMISSIONS: &[(&str, &str)] = &[
    ("plan_code", "Plan code changes for a work item"),
    ("plan_tests", "Plan tests for requirements"),
    ("author_new_code", "Propose new code nodes"),
    ("modify_code", "Propose new versions of existing code"),
    ("write_tests", "Propose new tests"),
    ("modify_tests", "Propose new versions of existing tests"),
    ("review_code", "Blind-review a work item's proposal"),
    ("run_tests", "Run tests"),
    ("build", "Request builds"),
    ("deploy", "Request deployments of project versions"),
    ("create_nodes", "Plan contexts, containers and components"),
    (
        "modify_requirements",
        "Propose requirement changes (a human approves them)",
    ),
    ("set_philosophy", "Rewrite the philosophy document"),
    ("create_work_items", "Queue work items"),
    ("close_work_items", "Complete or cancel work items"),
    ("schedule_tests", "Change test schedules"),
    ("set_caps", "Change agent concurrency caps"),
    ("report_blockers", "Notify humans of external blockers"),
    ("check_use_cases", "Decide use-case contradiction checks"),
    (
        "map_imports",
        "Map an imported repository onto the C4 graph",
    ),
    ("record_decisions", "Record decisions"),
];

/// A built-in agent type.
#[derive(Debug, Clone, Serialize)]
pub struct Builtin {
    pub name: &'static str,
    /// Prompt template file in `kodehub-agents/prompts`.
    pub prompt: &'static str,
    /// Tool role (`coder`, `tester`, `reviewer`, `orchestrator`).
    pub tool_role: &'static str,
    pub description: &'static str,
    pub permissions: &'static [&'static str],
}

pub const BUILTIN: &[Builtin] = &[
    Builtin {
        name: "coding",
        prompt: "coder",
        tool_role: "coder",
        description: "Plans and changes code at the code-node level, through work items.",
        permissions: &[
            "plan_code",
            "author_new_code",
            "modify_code",
            "run_tests",
            "build",
            "deploy",
            "create_work_items",
            "close_work_items",
            "record_decisions",
        ],
    },
    Builtin {
        name: "test_author",
        prompt: "test_author",
        tool_role: "tester",
        description: "Writes deterministic shell-run tests and owns the test schedule.",
        permissions: &[
            "plan_tests",
            "write_tests",
            "modify_tests",
            "run_tests",
            "schedule_tests",
            "create_work_items",
        ],
    },
    Builtin {
        name: "review",
        prompt: "reviewer",
        tool_role: "reviewer",
        description: "Blind code review: requirements, philosophy, diff and tests only.",
        permissions: &["review_code", "run_tests", "create_work_items"],
    },
    Builtin {
        name: "planning",
        prompt: "planning",
        tool_role: "orchestrator",
        description: "Breaks direction and requirements into nodes and work items.",
        permissions: &[
            "plan_code",
            "plan_tests",
            "create_nodes",
            "create_work_items",
            "close_work_items",
        ],
    },
    Builtin {
        name: "architect",
        prompt: "architect",
        tool_role: "orchestrator",
        description: "Higher-level decisions: hard failures, conflicts, deadlocks, use cases.",
        permissions: &[
            "create_nodes",
            "modify_requirements",
            "set_philosophy",
            "check_use_cases",
            "record_decisions",
            "create_work_items",
            "close_work_items",
            "report_blockers",
        ],
    },
    Builtin {
        name: "import",
        prompt: "import",
        tool_role: "orchestrator",
        description: "Maps an imported repository onto the C4 graph.",
        permissions: &["map_imports", "create_nodes", "create_work_items"],
    },
    Builtin {
        name: "design",
        prompt: "design",
        tool_role: "orchestrator",
        description: "UI designs that satisfy use cases, handed to coding agents.",
        permissions: &["plan_code", "create_nodes", "create_work_items"],
    },
    Builtin {
        name: "sysadmin",
        prompt: "sysadmin",
        tool_role: "orchestrator",
        description: "Sets agent caps to fit the cluster.",
        permissions: &["set_caps", "report_blockers"],
    },
];

pub fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTIN.iter().find(|b| b.name == name)
}

/// The document key of an agent type (`custom:policy` → `custom-policy`).
pub fn agent_key(name: &str) -> String {
    name.replace(':', "-")
}

impl Store {
    /// Seed the built-in agent types into the active project (idempotent).
    pub async fn ensure_agents(&self) -> Result<()> {
        for b in BUILTIN {
            let key = self.db.pkey(b.name)?;
            if self.db.get("agents", &key, None).await.is_ok() {
                continue;
            }
            self.db
                .insert_ignore(
                    "agents",
                    &json!({"_key": key, "name": b.name, "builtin": true, "prompt_file": b.prompt,
                            "tool_role": b.tool_role, "description": b.description,
                            "permissions": b.permissions, "prompt": null, "model": null,
                            "created_at": now_iso()}),
                    None,
                )
                .await?;
            self.own(&format!("agents/{key}")).await?;
        }
        Ok(())
    }

    /// One agent type of the active project.
    pub async fn agent(&self, name: &str) -> Result<Value> {
        self.ensure_agents().await?;
        self.db
            .get("agents", &self.db.pkey(&agent_key(name))?, None)
            .await
            .map_err(|_| StoreError::NotFound(format!("agent type `{name}`")))
    }

    pub async fn list_agents(&self) -> Result<Vec<Value>> {
        self.ensure_agents().await?;
        self.aql(
            "FOR a IN @@agents SORT a.builtin DESC, a.name RETURN a",
            json!({}),
        )
        .await
    }

    /// Create a custom agent type or change one: its start-up `prompt` (replaces the built-in
    /// prompt's role text), `permissions`, `description`, `model`.
    pub async fn put_agent(&self, name: &str, patch: &Value, by: &str) -> Result<Value> {
        self.ensure_agents().await?;
        let name = name.trim();
        let key = self.db.pkey(&agent_key(name))?;
        if let Some(perms) = patch.get("permissions") {
            let list = perms
                .as_array()
                .ok_or_else(|| StoreError::Invalid("permissions is a list".into()))?;
            for p in list {
                let p = p.as_str().unwrap_or("");
                if !PERMISSIONS.iter().any(|(n, _)| *n == p) {
                    return Err(StoreError::Invalid(format!("unknown permission `{p}`")));
                }
            }
        }
        let mut doc = serde_json::Map::new();
        for f in [
            "prompt",
            "permissions",
            "description",
            "model",
            "instructions",
        ] {
            if let Some(v) = patch.get(f) {
                doc.insert(f.into(), v.clone());
            }
        }
        doc.insert("updated_at".into(), json!(now_iso()));
        doc.insert("updated_by".into(), json!(by));
        match self.db.get("agents", &key, None).await {
            Ok(_) => {
                self.db
                    .patch("agents", &key, &Value::Object(doc), None)
                    .await?;
            }
            Err(_) => {
                if !name.starts_with("custom:") && !name.starts_with("custom-") {
                    return Err(StoreError::Invalid(
                        "a new agent type is named custom:<name>".into(),
                    ));
                }
                if patch["prompt"].as_str().unwrap_or("").trim().is_empty()
                    && patch["instructions"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .is_empty()
                {
                    return Err(StoreError::Invalid(
                        "a custom agent type needs a prompt".into(),
                    ));
                }
                doc.insert("_key".into(), json!(key));
                doc.insert("name".into(), json!(name.replace("custom-", "custom:")));
                doc.insert("builtin".into(), json!(false));
                doc.insert("prompt_file".into(), json!("custom"));
                doc.insert("tool_role".into(), json!("orchestrator"));
                if !doc.contains_key("permissions") {
                    doc.insert("permissions".into(), json!(["create_work_items"]));
                }
                doc.insert("created_at".into(), json!(now_iso()));
                self.db.insert("agents", &Value::Object(doc), None).await?;
                self.own(&format!("agents/{key}")).await?;
            }
        }
        self.db.get("agents", &key, None).await
    }

    /// May an agent type take an action?
    pub async fn agent_may(&self, agent_type: &str, permission: &str) -> Result<bool> {
        let a = self.agent(agent_type).await?;
        Ok(a["permissions"]
            .as_array()
            .is_some_and(|ps| ps.iter().any(|p| p == permission)))
    }

    /// Refuse an action outside an agent type's permissions.
    pub async fn require_agent_permission(&self, agent_type: &str, permission: &str) -> Result<()> {
        if self.agent_may(agent_type, permission).await? {
            Ok(())
        } else {
            Err(StoreError::Invalid(format!(
                "the {agent_type} agent type may not {permission} in this project"
            )))
        }
    }
}
