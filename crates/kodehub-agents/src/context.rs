//! Builds agent inputs from the graph. The reviewer's input is assembled only from
//! requirements, mission, diff and tests (blind review, doc 04).

use std::collections::BTreeSet;

use kodehub_core::model::Kind;
use kodehub_core::CodeGraph;
use kodehub_store::Store;
use serde_json::{json, Value};

use crate::Result;

/// The project's philosophy document (spec §7.1), the tiebreaker for every agent.
pub async fn mission(store: &Store) -> Result<String> {
    Ok(store
        .get_philosophy()
        .await?
        .and_then(|p| p["text"].as_str().map(str::to_string))
        .unwrap_or_else(|| "(no philosophy document)".to_string()))
}

fn render_examples(label: &str, v: &Value) -> String {
    let items = v.as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        return format!("- {label}: (none)\n");
    }
    let mut s = format!("- {label}:\n");
    for e in items {
        s.push_str(&format!(
            "  - {} — input `{}` → expected `{}`\n",
            e["description"].as_str().unwrap_or(""),
            e["input"],
            e["expected"]
        ));
    }
    s
}

fn render_list(label: &str, v: &Value) -> String {
    let items: Vec<String> = v
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    if items.is_empty() {
        return format!("- {label}: (none)\n");
    }
    format!(
        "- {label}:\n{}",
        items
            .iter()
            .map(|i| format!("  - {i}\n"))
            .collect::<String>()
    )
}

/// Full anatomy of a requirement in Markdown.
pub fn requirement_md(r: &Value) -> String {
    format!(
        "### {} (key `{}`, revision {})\n- statement: {}\n- intent: {}\n{}{}{}{}",
        r["title"].as_str().unwrap_or(""),
        r["_key"].as_str().unwrap_or(""),
        r["revision"],
        r["statement"].as_str().unwrap_or(""),
        r["intent"].as_str().unwrap_or(""),
        render_list("constraints", &r["constraints"]),
        render_examples("examples", &r["examples"]),
        render_examples("edge cases", &r["edge_cases"]),
        render_list("out of scope", &r["out_of_scope"]),
    )
}

/// Requirements of a changeset with "what changed" since the previous revision.
pub async fn requirements_md(store: &Store, keys: &[String], with_changes: bool) -> Result<String> {
    let mut out = String::new();
    for k in keys {
        let r = store.db.get("requirements", k, None).await?;
        out.push_str(&requirement_md(&r));
        if with_changes {
            let hist = store.requirement_history(k).await?;
            if let Some(prev) = hist.first() {
                let snap = &prev["snapshot"];
                let mut changes = Vec::new();
                for f in [
                    "statement",
                    "intent",
                    "constraints",
                    "examples",
                    "edge_cases",
                    "out_of_scope",
                ] {
                    if snap[f] != r[f] {
                        changes.push(format!(
                            "  - `{f}` changed from `{}` to `{}`",
                            snap[f], r[f]
                        ));
                    }
                }
                if !changes.is_empty() {
                    out.push_str(&format!(
                        "- **changed since revision {}:**\n{}\n",
                        prev["revision"],
                        changes.join("\n")
                    ));
                }
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// Symbols relevant to a set of requirements: satisfiers (and everything they reach) plus tests
/// that verify them.
pub async fn relevant_symbols(
    store: &Store,
    g: &CodeGraph,
    reqs: &[String],
) -> Result<BTreeSet<String>> {
    let ids: Vec<String> = reqs
        .iter()
        .map(|r| store.db.pid("requirements", kodehub_store::key_of(r)))
        .collect::<std::result::Result<_, _>>()?;
    // what the requirements apply to (and everything beneath, in the identity hierarchy and
    // over build edges), plus the tests verifying them
    let rows = store
        .aql(
            "LET at = (FOR e IN @@applies_to FILTER e._from IN @r RETURN e._to)
             LET reach = UNIQUE(FOR s IN at FOR v IN 0..30 OUTBOUND s @@contains, @@calls, @@depends_on, @@member_of
                                  OPTIONS {uniqueVertices: 'global', order: 'bfs'}
                                  FILTER PARSE_IDENTIFIER(v._id).collection IN ['code', 'tests'] RETURN v._key)
             LET tests = (FOR e IN @@verified_by FILTER e._from IN @r RETURN PARSE_IDENTIFIER(e._to).key)
             RETURN UNIQUE(APPEND(reach, tests))",
            json!({"r": ids}),
        )
        .await?;
    let mut set: BTreeSet<String> = rows
        .first()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    // add build closures so helpers are visible
    let roots: Vec<String> = set
        .iter()
        .filter(|k| g.symbols.contains_key(*k))
        .cloned()
        .collect();
    if let Ok(cl) = g.closure(&roots, false) {
        set.extend(cl.into_keys());
    }
    Ok(set
        .into_iter()
        .filter(|k| g.symbols.contains_key(k))
        .collect())
}

/// Code listing. `bodies = false` shows only signatures plus data shapes (struct/enum bodies) —
/// the tester's view.
pub fn code_md(
    g: &CodeGraph,
    symbols: &BTreeSet<String>,
    bodies: bool,
    include_tests: bool,
) -> String {
    let mut out = String::new();
    let order = g.topo_order(symbols);
    for id in order {
        let s = &g.symbols[&id];
        if s.kind == Kind::Test && !include_tests {
            continue;
        }
        let Ok(v) = g.version(&s.version) else {
            continue;
        };
        let show_body = bodies || s.kind.is_type() || s.kind == Kind::Test;
        let class = g
            .class_of(&id)
            .map(|c| format!(", member_of `{}`", c.id))
            .unwrap_or_default();
        out.push_str(&format!(
            "#### `{}` — {} {} (key `{}`{class})\n- signature: `{}`\n",
            s.display_name,
            s.language.as_str(),
            s.kind.as_str(),
            id,
            v.signature
        ));
        if v.attributes
            .as_object()
            .map(|o| !o.is_empty())
            .unwrap_or(false)
        {
            out.push_str(&format!("- attributes: `{}`\n", v.attributes));
        }
        if show_body {
            let lang = s.language.as_str();
            out.push_str(&format!("```{lang}\n{}\n```\n", v.body));
        }
        out.push('\n');
    }
    out
}

/// Blind-review diff: old → new for every proposal. Built from version content only.
pub fn diff_md(current: &CodeGraph, proposals: &Value) -> String {
    let mut out = String::new();
    for (k, p) in proposals.as_object().cloned().unwrap_or_default() {
        let lang = p["language"].as_str().unwrap_or("");
        let kind = p["kind"].as_str().unwrap_or("");
        if kind == "test" {
            continue;
        }
        let name = p["display_name"].as_str().unwrap_or(&k);
        match current.symbols.get(&k).and_then(|s| current.version(&s.version).ok()) {
            Some(old) if p["new"] != json!(true) => out.push_str(&format!(
                "#### MODIFIED `{name}` ({lang} {kind})\nold signature: `{}`\nnew signature: `{}`\nold:\n```{lang}\n{}\n```\nnew:\n```{lang}\n{}\n```\n\n",
                old.signature, p["signature"].as_str().unwrap_or(""), old.body, p["body"].as_str().unwrap_or("")
            )),
            _ => out.push_str(&format!(
                "#### ADDED `{name}` ({lang} {kind})\nsignature: `{}`\n```{lang}\n{}\n```\n\n",
                p["signature"].as_str().unwrap_or(""), p["body"].as_str().unwrap_or("")
            )),
        }
    }
    if out.is_empty() {
        out.push_str("(no code changes)\n");
    }
    out
}

/// Tests relevant to the review: proposed tests plus existing tests of the requirements.
pub fn tests_md(g: &CodeGraph, proposals: &Value, existing: &BTreeSet<String>) -> String {
    let mut out = String::new();
    let mut shown: BTreeSet<String> = BTreeSet::new();
    for (k, p) in proposals.as_object().cloned().unwrap_or_default() {
        if p["kind"] != json!("test") {
            continue;
        }
        let lang = p["language"].as_str().unwrap_or("");
        let name = p["display_name"].as_str().unwrap_or(&k).to_string();
        let old = g.symbols.get(&k).and_then(|s| g.version(&s.version).ok());
        match old {
            Some(o) if p["new"] != json!(true) => out.push_str(&format!(
                "#### MODIFIED TEST `{name}` ({lang}) verifies {:?}\nold:\n```{lang}\n{}\n```\nnew:\n```{lang}\n{}\n```\n\n",
                p["verifies"], o.body, p["body"].as_str().unwrap_or("")
            )),
            _ => out.push_str(&format!(
                "#### NEW TEST `{name}` ({lang}) verifies {:?}\n```{lang}\n{}\n```\n\n",
                p["verifies"], p["body"].as_str().unwrap_or("")
            )),
        }
        shown.insert(k);
    }
    for id in existing {
        if shown.contains(id) {
            continue;
        }
        if let Some(s) = g.symbols.get(id) {
            if s.kind == Kind::Test {
                if let Ok(v) = g.version(&s.version) {
                    out.push_str(&format!(
                        "#### EXISTING TEST `{}` ({})\n```{}\n{}\n```\n\n",
                        s.display_name,
                        s.language.as_str(),
                        s.language.as_str(),
                        v.body
                    ));
                }
            }
        }
    }
    out
}
