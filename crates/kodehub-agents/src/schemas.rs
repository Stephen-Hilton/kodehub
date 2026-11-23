//! JSON schemas for each agent's final answer (used for `--json-schema` / `output_config`).
//! Every property is required and `additionalProperties` is false so structured output is strict;
//! optional values are empty strings / lists.

use serde_json::{json, Value};

fn obj(props: Value) -> Value {
    let required: Vec<String> = props
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}
fn s() -> Value {
    json!({"type": "string"})
}
fn arr(items: Value) -> Value {
    json!({"type": "array", "items": items})
}
fn en(vals: &[&str]) -> Value {
    json!({"type": "string", "enum": vals})
}

pub fn plan() -> Value {
    obj(json!({
        "summary": s(),
        "changes": arr(obj(json!({
            "symbol": s(), "display_name": s(), "kind": en(&["function", "class", "enum", "trait", "test"]),
            "language": en(&["rust", "python"]), "action": en(&["add", "modify"]), "component": s(), "description": s(),
        }))),
        "tests": arr(s()),
        "risks": arr(s()),
    }))
}

pub fn tests() -> Value {
    obj(json!({
        "tests": arr(obj(json!({
            "display_name": s(), "language": en(&["rust", "python"]), "body": s(),
            "verifies": arr(s()), "tests_for": arr(s()), "description": s(),
        }))),
        "notes": s(),
    }))
}

pub fn code() -> Value {
    obj(json!({
        "changes": arr(obj(json!({
            "symbol": s(), "display_name": s(), "kind": en(&["function", "class", "enum", "trait"]),
            "language": en(&["rust", "python"]), "signature": s(), "body": s(),
            "attributes_json": s(), "component": s(), "member_of": s(), "description": s(),
        }))),
        "responses": arr(obj(json!({"finding_id": s(), "action": en(&["fixed", "disputed"]), "explanation": s()}))),
        "notes": s(),
    }))
}

pub fn review() -> Value {
    obj(json!({
        "verdict": en(&["approve", "changes_requested"]),
        "summary": s(),
        "findings": arr(obj(json!({
            "id": s(), "severity": en(&["blocking", "major", "minor", "info"]), "location": s(),
            "finding": s(), "status": en(&["open", "resolved"]),
        }))),
    }))
}

pub fn chat() -> Value {
    obj(json!({
        "reply": s(),
        "action": en(&["none", "add_edge_case", "add_example", "create_requirement", "open_changeset"]),
        "requirement": s(),
        "description": s(),
        "input_json": s(),
        "expected_json": s(),
        "title": s(),
        "statement": s(),
    }))
}
