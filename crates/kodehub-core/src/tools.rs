//! The graph API tool catalog. One definition drives the MCP server, the API agent backend and
//! documentation, so HTTP and MCP stay identical (doc 04 "Graph API").

use serde::Serialize;
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    /// HTTP method and path template; `{x}` placeholders are filled from (and removed from) args.
    pub method: &'static str,
    pub path: &'static str,
    /// Safe for read-only agents.
    pub read_only: bool,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

pub fn catalog() -> Vec<ToolDef> {
    let s = |d: &str| json!({"type": "string", "description": d});
    vec![
        ToolDef { name: "get_node", description: "Fetch one node by id (collection/key), e.g. code/verify_credentials or requirements/r1-generic-failure.",
            input_schema: obj(json!({"id": s("collection/key")}), &["id"]), method: "GET", path: "/api/node?id={id}", read_only: true },
        ToolDef { name: "query_nodes", description: "List documents in a collection, optionally filtered by exact field values. Collections (docs/ontology.md): philosophy, requirements, requirement_versions, use_cases, contexts, containers, components, code, tests, code_versions, test_versions, project_versions, environments, deployments, test_logs, builds, agents.",
            input_schema: obj(json!({"collection": s("collection name"), "filter": {"type": "object", "description": "field → exact value"}, "limit": {"type": "integer"}}), &["collection"]),
            method: "POST", path: "/api/query", read_only: true },
        ToolDef { name: "traverse", description: "Traverse the graph from a node. direction: outbound|inbound|any. edges: edge collection names (e.g. calls, depends_on, contains, applies_to, verified_by, tested_with); empty = all.",
            input_schema: obj(json!({"start": s("node id"), "direction": s("outbound|inbound|any"), "edges": {"type": "array", "items": {"type": "string"}}, "depth": {"type": "integer"}}), &["start"]),
            method: "POST", path: "/api/traverse", read_only: true },
        ToolDef { name: "get_symbol", description: "Code or test node detail: its version in the current project version (signature, body), version history, requirements it serves, dependencies, tests.",
            input_schema: obj(json!({"key": s("code or test key, e.g. verify_credentials")}), &["key"]), method: "GET", path: "/api/symbols/{key}", read_only: true },
        ToolDef { name: "list_requirements", description: "All accepted requirements with their full anatomy, tags and coverage status.",
            input_schema: obj(json!({}), &[]), method: "GET", path: "/api/requirements", read_only: true },
        // Requirements are human-owned: agents propose changes through the orchestrator chat,
        // which a human must Apply. There are no direct requirement-editing tools (final #8).
        ToolDef { name: "create_decision", description: "Record a decision (a requirement of kind decision: title, choice, rationale, alternatives[], made_by) applying to the given node ids.",
            input_schema: obj(json!({"title": s("title"), "choice": s("what was chosen"), "rationale": s("why"), "alternatives": {"type": "array"}, "made_by": s("human or agent id"), "justifies": {"type": "array", "items": {"type": "string"}, "description": "node ids the decision applies to"}}), &["title", "choice", "rationale"]),
            method: "POST", path: "/api/decisions", read_only: false },
        ToolDef { name: "list_proposals", description: "Work-item proposals (the code changes in flight) with state, plan, proposed versions and findings. open=true leaves out accepted and rejected ones.",
            input_schema: obj(json!({"open": {"type": "boolean"}}), &[]), method: "GET", path: "/api/proposals?open={open}", read_only: true },
        ToolDef { name: "open_proposal", description: "Open a work item carrying a code-change proposal for some intent; requirement keys optional. Returns the proposal; its key is the work item key.",
            input_schema: obj(json!({"title": s("title"), "intent": s("what should change and why"), "requirements": {"type": "array", "items": {"type": "string"}}}), &["title"]),
            method: "POST", path: "/api/proposals", read_only: false },
        ToolDef { name: "get_proposal", description: "One work item's proposal.",
            input_schema: obj(json!({"work_item": s("work item key")}), &["work_item"]), method: "GET", path: "/api/work-items/{work_item}/proposal", read_only: true },
        ToolDef { name: "propose_version", description: "Propose a new version of a code or test node (or a new node) in a work item's proposal. Nothing changes in the project version until the proposal is accepted.",
            input_schema: obj(json!({"work_item": s("work item key"), "symbol": s("existing code/test key, or omit for a new node"), "display_name": s("for new nodes"), "kind": s("function|class|enum|trait|test"), "language": s("rust|python"), "signature": s("name-free signature"), "body": s("body without braces/def line"), "attributes": {"type": "object"}, "component": s("component key for new nodes"), "verifies": {"type": "array", "items": {"type": "string"}}}), &["work_item", "signature", "body"]),
            method: "POST", path: "/api/work-items/{work_item}/proposal/versions", read_only: false },
        ToolDef { name: "propose_edge", description: "Propose an edge (calls, depends_on, member_of, binds_to, tested_with, verified_by) in a work item's proposal (applied on acceptance).",
            input_schema: obj(json!({"work_item": s("work item key"), "collection": s("edge collection"), "from": s("node id"), "to": s("node id")}), &["work_item", "collection", "from", "to"]),
            method: "POST", path: "/api/work-items/{work_item}/proposal/edges", read_only: false },
        ToolDef { name: "submit_review", description: "Submit advisory review findings for a proposal (findings[] of {severity: blocking|major|minor|info, location, finding}). Only the engine's blind reviewer or a human in the app can satisfy the acceptance review check.",
            input_schema: obj(json!({"work_item": s("work item key"), "findings": {"type": "array"}, "summary": s("summary")}), &["work_item", "findings"]),
            method: "POST", path: "/api/work-items/{work_item}/proposal/reviews", read_only: false },
        ToolDef { name: "accept_proposal", description: "Accept a ready proposal into its project version, all or nothing (checks: no orphans introduced, affected tests pass, review passed, no unresolved blocking findings).",
            input_schema: obj(json!({"work_item": s("work item key")}), &["work_item"]), method: "POST", path: "/api/work-items/{work_item}/proposal/accept", read_only: false },
        ToolDef { name: "reject_proposal", description: "Reject a proposal.",
            input_schema: obj(json!({"work_item": s("work item key"), "reason": s("why")}), &["work_item"]), method: "POST", path: "/api/work-items/{work_item}/proposal/reject", read_only: false },
        ToolDef { name: "coverage", description: "Coverage: every requirement (or one) with pass/fail/at_risk/uncovered and the tests behind it, plus coverage per tag.",
            input_schema: obj(json!({"requirement": s("optional requirement key")}), &[]), method: "GET", path: "/api/coverage?requirement={requirement}", read_only: true },
        ToolDef { name: "orphans", description: "Code no requirement reaches.",
            input_schema: obj(json!({}), &[]), method: "GET", path: "/api/orphans", read_only: true },
        ToolDef { name: "impact", description: "Everything at risk if a code node changes: dependants, C4 nodes, tests, requirements, tags.",
            input_schema: obj(json!({"symbol": s("code key")}), &["symbol"]), method: "GET", path: "/api/impact/{symbol}", read_only: true },
        ToolDef { name: "duplicates", description: "Code whose versions in the current project version share a content hash (merge candidates).",
            input_schema: obj(json!({}), &[]), method: "GET", path: "/api/duplicates", read_only: true },
        ToolDef { name: "run_tests", description: "Run tests. scope: {kind: all} | {kind: requirement, id} | {kind: symbol, id} | {kind: tests, id: [..]}.",
            input_schema: obj(json!({"scope": {"type": "object"}, "force": {"type": "boolean"}}), &[]), method: "POST", path: "/api/tests/run", read_only: false },
        ToolDef { name: "run_now", description: "Run a function with JSON args in an environment (default dev). Instant on cache hit.",
            input_schema: obj(json!({"symbol": s("code key"), "args": {"type": "object"}, "environment": s("dev|test|qa|prod")}), &["symbol"]),
            method: "POST", path: "/api/run", read_only: false },
        ToolDef { name: "list_project_versions", description: "Project versions (the releases), newest first: open or frozen, what they are based on, where each runs.",
            input_schema: obj(json!({}), &[]), method: "GET", path: "/api/project-versions", read_only: true },
        ToolDef { name: "new_project_version", description: "Open a new project version (the next release) starting from another's tree (default: the current one), and make it current.",
            input_schema: obj(json!({"name": s("optional name"), "based_on": s("project version key")}), &[]), method: "POST", path: "/api/project-versions", read_only: false },
        ToolDef { name: "freeze_project_version", description: "Freeze a project version: it never changes again. A fix after that is a hotfix version.",
            input_schema: obj(json!({"project_version": s("project version key"), "reason": s("why")}), &["project_version"]), method: "POST", path: "/api/project-versions/{project_version}/freeze", read_only: false },
        ToolDef { name: "deploy", description: "Deploy a project version into an environment (gated; promotions go one environment up at a time). Deploying into production waits for a human approval given in the app.",
            input_schema: obj(json!({"project_version": s("project version key"), "environment": s("environment name"), "reason": s("promote (default) or hotfix")}), &["project_version", "environment"]),
            method: "POST", path: "/api/deployments", read_only: false },
        ToolDef { name: "list_deployments", description: "Deployments (newest first), optionally of one environment, with state and logs.",
            input_schema: obj(json!({"environment": s("optional environment")}), &[]), method: "GET", path: "/api/deployments?environment={environment}", read_only: true },
        ToolDef { name: "rollback", description: "Roll an environment back to the project version it previously ran.",
            input_schema: obj(json!({"environment": s("environment")}), &["environment"]), method: "POST", path: "/api/environments/{environment}/rollback", read_only: false },
    ]
}

/// Turn a tool call into (method, path, body) by filling path placeholders from args.
pub fn to_request(def: &ToolDef, args: &Value) -> (String, String, Option<Value>) {
    let mut rest: Map<String, Value> = args.as_object().cloned().unwrap_or_default();
    let mut path = def.path.to_string();
    while let Some(start) = path.find('{') {
        let end = path[start..]
            .find('}')
            .map(|e| start + e)
            .unwrap_or(path.len() - 1);
        let name = path[start + 1..end].to_string();
        let val = rest.remove(&name).map(|v| match v {
            Value::String(s) => s,
            Value::Null => String::new(),
            other => other.to_string(),
        });
        let enc = val.map(|v| encode(&v)).unwrap_or_default();
        path.replace_range(start..=end, &enc);
    }
    // drop empty query params
    if let Some((base, q)) = path.split_once('?') {
        let kept: Vec<&str> = q.split('&').filter(|kv| !kv.ends_with('=')).collect();
        path = if kept.is_empty() {
            base.to_string()
        } else {
            format!("{base}?{}", kept.join("&"))
        };
    }
    let body = if def.method == "GET" {
        None
    } else {
        Some(Value::Object(rest))
    };
    (def.method.to_string(), path, body)
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requests_fill_paths() {
        let c = catalog();
        let get = |n| c.iter().find(|t| t.name == n).unwrap();
        assert_eq!(
            to_request(get("coverage"), &json!({})),
            ("GET".into(), "/api/coverage".into(), None)
        );
        assert_eq!(
            to_request(get("coverage"), &json!({"requirement": "r1"})).1,
            "/api/coverage?requirement=r1"
        );
        let (m, p, b) = to_request(
            get("propose_version"),
            &json!({"work_item": "wi-1", "body": "x", "signature": "()"}),
        );
        assert_eq!(
            (m.as_str(), p.as_str()),
            ("POST", "/api/work-items/wi-1/proposal/versions")
        );
        assert_eq!(
            to_request(get("list_proposals"), &json!({"open": true})).1,
            "/api/proposals?open=true"
        );
        assert_eq!(b.unwrap(), json!({"body": "x", "signature": "()"}));
        let names: std::collections::BTreeSet<_> = c.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), c.len(), "tool names unique");
    }
}
