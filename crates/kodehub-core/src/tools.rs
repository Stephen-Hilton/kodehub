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
        ToolDef { name: "get_node", description: "Fetch one node by id (collection/key), e.g. symbols/verify_credentials or requirements/r1-generic-failure.",
            input_schema: obj(json!({"id": s("collection/key")}), &["id"]), method: "GET", path: "/api/node?id={id}", read_only: true },
        ToolDef { name: "query_nodes", description: "List documents in a collection, optionally filtered by exact field values. Collections: missions, requirements, requirement_groups, decisions, contexts, containers, components, symbols, versions, changesets, releases, environments, test_runs, runs.",
            input_schema: obj(json!({"collection": s("collection name"), "filter": {"type": "object", "description": "field → exact value"}, "limit": {"type": "integer"}}), &["collection"]),
            method: "POST", path: "/api/query", read_only: true },
        ToolDef { name: "traverse", description: "Traverse the graph from a node. direction: outbound|inbound|any. edges: edge collection names (e.g. calls, depends_on, contains, satisfies, verifies, tested_by); empty = all.",
            input_schema: obj(json!({"start": s("node id"), "direction": s("outbound|inbound|any"), "edges": {"type": "array", "items": {"type": "string"}}, "depth": {"type": "integer"}}), &["start"]),
            method: "POST", path: "/api/traverse", read_only: true },
        ToolDef { name: "get_symbol", description: "Symbol detail: current version (signature, body), version history, requirements it serves, dependencies, tests.",
            input_schema: obj(json!({"key": s("symbol key, e.g. verify_credentials")}), &["key"]), method: "GET", path: "/api/symbols/{key}", read_only: true },
        ToolDef { name: "list_requirements", description: "All requirements with their full anatomy and coverage status.",
            input_schema: obj(json!({}), &[]), method: "GET", path: "/api/requirements", read_only: true },
        // Requirements are human-owned: agents propose changes through the orchestrator chat,
        // which a human must Apply. There are no direct requirement-editing tools (final #8).
        ToolDef { name: "create_decision", description: "Record a decision (title, choice, rationale, alternatives[], made_by) and link it with justified_by from the given node ids.",
            input_schema: obj(json!({"title": s("title"), "choice": s("what was chosen"), "rationale": s("why"), "alternatives": {"type": "array"}, "made_by": s("human or agent id"), "justifies": {"type": "array", "items": {"type": "string"}}}), &["title", "choice", "rationale"]),
            method: "POST", path: "/api/decisions", read_only: false },
        ToolDef { name: "list_changesets", description: "Changesets with state, plan, proposals and findings.",
            input_schema: obj(json!({}), &[]), method: "GET", path: "/api/changesets", read_only: true },
        ToolDef { name: "open_changeset", description: "Open a changeset (graph PR) for some intent; requirement keys optional.",
            input_schema: obj(json!({"title": s("title"), "intent": s("what should change and why"), "requirements": {"type": "array", "items": {"type": "string"}}}), &["title"]),
            method: "POST", path: "/api/changesets", read_only: false },
        ToolDef { name: "propose_version", description: "Propose a new version of a symbol (or a new symbol) inside a changeset. Nothing becomes current until merge.",
            input_schema: obj(json!({"changeset": s("changeset key"), "symbol": s("existing symbol key, or omit for a new symbol"), "display_name": s("for new symbols"), "kind": s("function|class|enum|trait|test"), "language": s("rust|python"), "signature": s("name-free signature"), "body": s("body without braces/def line"), "attributes": {"type": "object"}, "component": s("component key for new symbols"), "verifies": {"type": "array", "items": {"type": "string"}}}), &["changeset", "signature", "body"]),
            method: "POST", path: "/api/changesets/{changeset}/versions", read_only: false },
        ToolDef { name: "propose_edge", description: "Propose an edge inside a changeset (applied on merge).",
            input_schema: obj(json!({"changeset": s("changeset key"), "collection": s("edge collection"), "from": s("node id"), "to": s("node id")}), &["changeset", "collection", "from", "to"]),
            method: "POST", path: "/api/changesets/{changeset}/edges", read_only: false },
        ToolDef { name: "submit_review", description: "Submit advisory review findings for a changeset (findings[] of {severity: blocking|major|minor|info, location, finding}). Only the engine's blind reviewer or a human in the app can satisfy the merge review check.",
            input_schema: obj(json!({"changeset": s("changeset key"), "findings": {"type": "array"}, "summary": s("summary")}), &["changeset", "findings"]),
            method: "POST", path: "/api/changesets/{changeset}/reviews", read_only: false },
        ToolDef { name: "merge_changeset", description: "Merge a ready changeset (checks: no orphans introduced, affected tests pass, no unresolved blocking findings).",
            input_schema: obj(json!({"changeset": s("changeset key")}), &["changeset"]), method: "POST", path: "/api/changesets/{changeset}/merge", read_only: false },
        ToolDef { name: "reject_changeset", description: "Reject a changeset.",
            input_schema: obj(json!({"changeset": s("changeset key"), "reason": s("why")}), &["changeset"]), method: "POST", path: "/api/changesets/{changeset}/reject", read_only: false },
        ToolDef { name: "coverage", description: "Coverage: every requirement (or one) with pass/fail/at_risk/uncovered and the tests behind it.",
            input_schema: obj(json!({"requirement": s("optional requirement key")}), &[]), method: "GET", path: "/api/coverage?requirement={requirement}", read_only: true },
        ToolDef { name: "orphans", description: "Symbols no requirement reaches.",
            input_schema: obj(json!({}), &[]), method: "GET", path: "/api/orphans", read_only: true },
        ToolDef { name: "impact", description: "Everything at risk if a symbol changes: dependants, requirements, groups.",
            input_schema: obj(json!({"symbol": s("symbol key")}), &["symbol"]), method: "GET", path: "/api/impact/{symbol}", read_only: true },
        ToolDef { name: "duplicates", description: "Symbols whose current versions share a content hash (merge candidates).",
            input_schema: obj(json!({}), &[]), method: "GET", path: "/api/duplicates", read_only: true },
        ToolDef { name: "run_tests", description: "Run tests. scope: {kind: all} | {kind: requirement, id} | {kind: symbol, id} | {kind: tests, id: [..]}.",
            input_schema: obj(json!({"scope": {"type": "object"}, "force": {"type": "boolean"}}), &[]), method: "POST", path: "/api/tests/run", read_only: false },
        ToolDef { name: "run_now", description: "Run a function with JSON args in an environment (default dev). Instant on cache hit.",
            input_schema: obj(json!({"symbol": s("symbol key"), "args": {"type": "object"}, "environment": s("dev|test|qa|prod")}), &["symbol"]),
            method: "POST", path: "/api/run", read_only: false },
        ToolDef { name: "cut_release", description: "Cut an immutable release of the current graph.",
            input_schema: obj(json!({"name": s("optional name"), "notes": s("notes")}), &[]), method: "POST", path: "/api/releases", read_only: false },
        ToolDef { name: "promote", description: "Promote a release into an environment (gated). Promotion into prod needs a human approval given in the app.",
            input_schema: obj(json!({"release": s("release key"), "environment": s("test|qa|prod")}), &["release", "environment"]),
            method: "POST", path: "/api/environments/{environment}/promote", read_only: false },
        ToolDef { name: "rollback", description: "Roll an environment back to the release it previously ran.",
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
            &json!({"changeset": "cs-1", "body": "x", "signature": "()"}),
        );
        assert_eq!(
            (m.as_str(), p.as_str()),
            ("POST", "/api/changesets/cs-1/versions")
        );
        assert_eq!(b.unwrap(), json!({"body": "x", "signature": "()"}));
        let names: std::collections::BTreeSet<_> = c.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), c.len(), "tool names unique");
    }
}
