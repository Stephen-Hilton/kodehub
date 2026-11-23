//! Agent prompts live in `crates/kodehub-agents/prompts/*.md` (compiled in) and can be tuned
//! without rebuilding by dropping a file with the same name into `$KODEHUB_HOME/prompts/`.

use crate::Role;

const CONVENTIONS: &str = include_str!("../prompts/conventions.md");

fn builtin(role: Role) -> &'static str {
    match role {
        Role::Orchestrator => include_str!("../prompts/orchestrator.md"),
        Role::Coder => include_str!("../prompts/coder.md"),
        Role::Reviewer => include_str!("../prompts/reviewer.md"),
        Role::Tester => include_str!("../prompts/tester.md"),
        Role::Environment => include_str!("../prompts/environment.md"),
    }
}

pub fn system_prompt(role: Role) -> String {
    let override_path = kodehub_core::config::kodehub_home()
        .join("prompts")
        .join(format!("{}.md", role.as_str()));
    let base = std::fs::read_to_string(override_path).unwrap_or_else(|_| builtin(role).to_string());
    base.replace("{{CONVENTIONS}}", CONVENTIONS)
}
