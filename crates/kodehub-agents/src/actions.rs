//! Apply a role's actions through the store (the agent never writes the graph itself).
//!
//! Humans approve only what is theirs: new contexts (and containers under
//! `human_engagement = containers`) and every requirement change become pending approvals.

use kodehub_core::model::{Kind, Language};
use kodehub_store::proposals::ProposeVersion;
use kodehub_store::queue::{NewWorkItem, PRIORITY_NORMAL};
use kodehub_store::Store;
use serde_json::{json, Value};

use crate::Result;

pub struct ApplyCtx<'a> {
    pub store: &'a Store,
    pub project: &'a str,
    pub work_item: &'a Value,
    pub agent_type: &'a str,
}

fn list<'a>(a: &'a Value, k: &str) -> Vec<&'a Value> {
    match a.get(k).and_then(Value::as_array) {
        Some(v) => v.iter().collect(),
        None => vec![a],
    }
}

/// Apply every action; returns a report of what happened.
pub async fn apply(ctx: &ApplyCtx<'_>, actions: &[Value]) -> Result<Value> {
    let mut report = json!({"applied": [], "approvals": [], "enqueued": [], "blocked": null});
    let engagement = ctx
        .store
        .setting(ctx.project, "human_engagement")
        .await?
        .as_str()
        .unwrap_or("contexts")
        .to_string();
    let by = format!(
        "agent:{}:{}",
        ctx.agent_type,
        ctx.work_item["_key"].as_str().unwrap_or("")
    );
    for a in actions {
        let t = a["type"].as_str().unwrap_or("");
        // every action is checked against the agent type's permissions in this project (D-062)
        if let Some(perm) = permission_for(t) {
            ctx.store
                .require_agent_permission(ctx.agent_type, perm)
                .await?;
        }
        match t {
            "create_nodes" => {
                for n in list(a, "nodes") {
                    let level = n["level"].as_str().unwrap_or("component");
                    let needs_human = engagement != "none"
                        && (level == "context"
                            || (level == "container" && engagement == "containers"));
                    let mut n = n.clone();
                    n["planned_by"] = json!(by);
                    if needs_human {
                        let kind = if level == "context" {
                            "new_context"
                        } else {
                            "new_container"
                        };
                        let ap = ctx
                            .store
                            .request_approval(
                                kind,
                                n.clone(),
                                n["rationale"].as_str().unwrap_or(""),
                                &by,
                            )
                            .await?;
                        push(&mut report, "approvals", ap["_key"].clone());
                    } else {
                        let id = ctx.store.create_planned_node(&n).await?;
                        push(&mut report, "applied", json!({"created": id}));
                    }
                }
            }
            "enqueue" => {
                let items = list(a, "items");
                let mut made: Vec<String> = Vec::new();
                for it in items {
                    let deps: Vec<String> = it["after"]
                        .as_array()
                        .map(|x| {
                            x.iter()
                                .filter_map(|i| i.as_u64())
                                .filter_map(|i| made.get(i as usize).cloned())
                                .collect()
                        })
                        .unwrap_or_default();
                    let w = ctx
                        .store
                        .enqueue(&NewWorkItem {
                            project: ctx.project.to_string(),
                            agent_type: it["agent_type"].as_str().unwrap_or("coding").into(),
                            kind: it["kind"].as_str().unwrap_or("").into(),
                            target: it["target"].as_str().map(str::to_string),
                            priority: it["priority"].as_i64().unwrap_or(PRIORITY_NORMAL),
                            task: it["task"].clone(),
                            depends_on: deps,
                            requested_by: Some(by.clone()),
                            ..Default::default()
                        })
                        .await?;
                    let k = w["_key"].as_str().unwrap_or_default().to_string();
                    push(&mut report, "enqueued", json!(k));
                    made.push(k);
                }
            }
            "propose_requirement_change" => {
                let ap = ctx
                    .store
                    .request_approval(
                        "requirement_change",
                        a.clone(),
                        a["rationale"].as_str().unwrap_or(""),
                        &by,
                    )
                    .await?;
                push(&mut report, "approvals", ap["_key"].clone());
            }
            "set_philosophy" => {
                let text = a["text"].as_str().unwrap_or("").trim();
                if !text.is_empty() {
                    ctx.store.set_philosophy(text, &by).await?;
                    push(&mut report, "applied", json!({"philosophy": "updated"}));
                }
            }
            "write_tests" => {
                let k = write_tests(ctx, a, &by).await?;
                push(&mut report, "applied", json!({"tests_work_item": k}));
            }
            "schedule" => {
                for s in list(a, "schedules") {
                    let target = s["target"].as_str().unwrap_or("project");
                    let secs = s["interval_secs"].as_i64().unwrap_or(86400);
                    let level = s["level"].as_str().unwrap_or("node");
                    ctx.store
                        .upsert_schedule(ctx.project, target, level, secs, "test_author")
                        .await?;
                    push(
                        &mut report,
                        "applied",
                        json!({"schedule": target, "interval_secs": secs}),
                    );
                }
            }
            "set_caps" => {
                let caps = a["agent_caps"].clone();
                ctx.store
                    .set_setting_system("app", "agent_caps", caps.clone(), &by)
                    .await?;
                push(&mut report, "applied", json!({"agent_caps": caps}));
            }
            "external_blocker" => {
                let msg = a["message"]
                    .as_str()
                    .unwrap_or("an external blocker needs a human");
                ctx.store
                    .notify(
                        ctx.project,
                        "external_blocker",
                        msg,
                        json!({"work_item": ctx.work_item["_key"], "by": by}),
                    )
                    .await?;
                report["blocked"] = json!(msg);
            }
            "use_case_verdict" => {
                let uc = a["use_case"].as_str().unwrap_or("");
                let ok = a["ok"] == true;
                ctx.store
                    .use_case_verdict(
                        uc,
                        ok,
                        a["conflicts"].clone(),
                        a["notes"].as_str().unwrap_or(""),
                        &by,
                    )
                    .await?;
                push(&mut report, "applied", json!({"use_case": uc, "live": ok}));
            }
            "import_mapping" => {
                let wi = ctx.work_item["_key"].as_str().unwrap_or_default();
                let r = ctx.store.apply_import_mapping(a, wi, &by).await?;
                push(&mut report, "applied", r);
            }
            "note" | "" => {}
            other => {
                push(&mut report, "applied", json!({"ignored": other}));
            }
        }
    }
    Ok(report)
}

/// The permission an action needs (`note` needs none).
pub fn permission_for(action: &str) -> Option<&'static str> {
    Some(match action {
        "create_nodes" => "create_nodes",
        "enqueue" => "create_work_items",
        "propose_requirement_change" => "modify_requirements",
        "set_philosophy" => "set_philosophy",
        "write_tests" => "write_tests",
        "schedule" => "schedule_tests",
        "set_caps" => "set_caps",
        "external_blocker" => "report_blockers",
        "use_case_verdict" => "check_use_cases",
        "import_mapping" => "map_imports",
        _ => return None,
    })
}

fn push(report: &mut Value, k: &str, v: Value) {
    if let Some(a) = report[k].as_array_mut() {
        a.push(v);
    }
}

/// Tests before code (spec §14.3): the tests go in as the proposal of the test author's own
/// work item, accepted while the code does not exist yet; they fail until a coding agent
/// implements the component.
async fn write_tests(ctx: &ApplyCtx<'_>, a: &Value, by: &str) -> Result<String> {
    let component = a["component"]
        .as_str()
        .or(ctx.work_item["target"].as_str())
        .map(str::to_string);
    let reqs: Vec<String> = list(a, "tests")
        .iter()
        .flat_map(|t| t["verifies"].as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let wi = ctx.work_item["_key"].as_str().unwrap_or_default();
    let cs = ctx
        .store
        .open_proposal(
            wi,
            &format!(
                "Tests for {}",
                component.as_deref().unwrap_or("the project")
            ),
            "Tests written before code from the merged requirements (spec §14.3).",
            &reqs,
            by,
        )
        .await?;
    let key = cs["_key"].as_str().unwrap_or_default().to_string();
    ctx.store
        .patch_proposal(&key, json!({"tests_first": true}))
        .await?;
    for t in list(a, "tests") {
        let lang: Language =
            serde_json::from_value(t["language"].clone()).unwrap_or(Language::Rust);
        let mut attributes = json!({});
        if t["shell"] == true {
            attributes["shell"] = json!(true);
        }
        ctx.store
            .propose_version(
                &key,
                &ProposeVersion {
                    symbol: None,
                    display_name: t["display_name"].as_str().map(str::to_string),
                    kind: Some(Kind::Test),
                    language: Some(lang),
                    visibility: None,
                    signature: String::new(),
                    body: t["body"].as_str().unwrap_or("").into(),
                    attributes: Some(attributes),
                    component: component
                        .as_ref()
                        .map(|c| kodehub_store::key_of(c).to_string()),
                    member_of: None,
                    verifies: t["verifies"]
                        .as_array()
                        .map(|v| {
                            v.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    tests_for: t["tests_for"]
                        .as_array()
                        .map(|v| {
                            v.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    description: t["description"].as_str().map(str::to_string),
                    by: Some(by.to_string()),
                },
            )
            .await?;
    }
    ctx.store.accept_tests_first(&key, by).await?;
    Ok(key)
}
