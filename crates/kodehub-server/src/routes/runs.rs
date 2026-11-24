//! Tests and run-now.

use axum::extract::{Query, State};
use axum::Json;
use kodehub_core::CodeGraph;
use kodehub_runner::{RunRequest, TestScope};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

#[derive(Deserialize, Default)]
pub struct RunTestsBody {
    #[serde(default)]
    pub scope: Option<TestScope>,
    #[serde(default)]
    pub force: bool,
}

pub async fn run_tests(
    State(s): State<AppState>,
    body: Option<Json<RunTestsBody>>,
) -> ApiResult<Value> {
    let b = body.map(|b| b.0).unwrap_or_default();
    let g = s.store.load_code_graph().await?;
    let scope = b.scope.unwrap_or(TestScope::All);
    let ids = s.runner.resolve_scope(&s.store, &g, &scope).await?;
    let outcomes = s
        .runner
        .run_tests(&s.store, &g, &ids, b.force, None, None)
        .await?;
    for o in &outcomes {
        s.events.emit(
            "test.result",
            json!({"test": o.test, "name": o.name, "status": o.status, "cached": o.cached}),
        );
    }
    let passed = outcomes.iter().filter(|o| o.status == "pass").count();
    s.events.emit("node.changed", json!({"change": "tests"}));
    Ok(Json(json!({
        "results": outcomes,
        "summary": {"total": outcomes.len(), "passed": passed, "failed": outcomes.len() - passed,
                    "cached": outcomes.iter().filter(|o| o.cached).count()}
    })))
}

#[derive(Deserialize)]
pub struct LimitQuery {
    pub limit: Option<usize>,
    pub symbol: Option<String>,
}

pub async fn test_results(
    State(s): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Vec<Value>> {
    Ok(Json(
        kodehub_runner::Runner::latest_results(&s.store, q.limit.unwrap_or(100)).await?,
    ))
}

#[derive(Deserialize)]
pub struct RunBody {
    pub symbol: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub environment: Option<String>,
    /// Client token to correlate streamed `run.output` events.
    #[serde(default)]
    pub token: Option<String>,
}

/// The code graph an environment runs: dev = current working state; others = their release.
pub async fn graph_for_env(
    s: &AppState,
    env: &str,
) -> Result<(CodeGraph, Option<String>), ApiError> {
    if env == "dev" {
        return Ok((s.store.load_code_graph().await?, None));
    }
    match s.store.release_graph_for_env(env).await? {
        Some((rel, g)) => Ok((g, Some(rel))),
        None => Err(ApiError::bad_request(format!(
            "no release is running in `{env}` yet — promote one first"
        ))),
    }
}

pub async fn run_now(State(s): State<AppState>, Json(b): Json<RunBody>) -> ApiResult<Value> {
    let env = b.environment.unwrap_or_else(|| "dev".into());
    let key = crate::routes::graph::resolve_symbol(&s, &b.symbol).await?;
    let (g, release) = graph_for_env(&s, &env).await?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<kodehub_runner::OutputLine>();
    let ev = s.events.clone();
    let token = b.token.clone();
    let sym = key.clone();
    let fwd = kodehub_store::spawn_scoped(async move {
        while let Some(l) = rx.recv().await {
            if l.line.starts_with(kodehub_runner::run::RESULT_MARKER) {
                continue;
            }
            ev.emit(
                "run.output",
                json!({"symbol": sym, "token": token, "stream": l.stream, "line": l.line}),
            );
        }
    });
    let args = if b.args.is_null() { json!({}) } else { b.args };
    let rec = s
        .runner
        .run_now(
            &g,
            &RunRequest {
                symbol: key.clone(),
                args,
                environment: env.clone(),
                release,
            },
            Some(tx),
        )
        .await?;
    let _ = fwd.await;
    s.runner.record_run(&s.store, &rec).await?;
    s.events.emit(
        "run.output",
        json!({"symbol": key, "token": b.token, "done": true, "ok": rec.ok, "result": rec.result}),
    );
    Ok(Json(serde_json::to_value(rec).unwrap()))
}

pub async fn list_runs(
    State(s): State<AppState>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Vec<Value>> {
    Ok(Json(
        s.store
            .aql(
                "FOR r IN @@runs FILTER @s == null OR r.symbol == @s SORT r.ran_at DESC LIMIT @n RETURN r",
                json!({"s": q.symbol, "n": q.limit.unwrap_or(50)}),
            )
            .await?,
    ))
}
