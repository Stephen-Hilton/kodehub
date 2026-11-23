//! HTTP routes. Every operation in doc 04's graph API has a route here; MCP tools map onto
//! these 1:1 via `kodehub_core::tools`.

pub mod agents;
pub mod changesets;
pub mod graph;
pub mod releases;
pub mod requirements;
pub mod runs;
pub mod settings;
pub mod terminal;

use axum::routing::{get, patch, post};
use axum::Router;

use crate::AppState;

pub fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(graph::health))
        .route("/api/health", get(graph::health))
        .route("/api/events", get(crate::events::ws_events))
        // generic graph access
        .route("/api/node", get(graph::get_node))
        .route(
            "/api/nodes/{collection}",
            get(graph::list_nodes).post(graph::create_node),
        )
        .route(
            "/api/nodes/{collection}/{key}",
            get(graph::get_node_path).patch(graph::patch_node),
        )
        .route("/api/query", post(graph::query_nodes))
        .route("/api/traverse", post(graph::traverse))
        .route(
            "/api/edges/{collection}",
            post(graph::create_edge).delete(graph::delete_edge),
        )
        .route("/api/seed", post(graph::seed))
        .route("/api/invariants", get(graph::invariants))
        // code
        .route("/api/symbols", get(graph::list_symbols))
        .route("/api/symbols/{key}", get(graph::symbol_detail))
        .route("/api/versions/{hash}", get(graph::get_version))
        .route("/api/map", get(graph::map))
        .route("/api/timeline", get(graph::timeline))
        // traceability
        .route("/api/coverage", get(graph::coverage))
        .route("/api/orphans", get(graph::orphans))
        .route("/api/impact/{symbol}", get(graph::impact))
        .route("/api/duplicates", get(graph::duplicates))
        .route("/api/health/nodes", get(graph::node_health))
        // requirements & decisions
        .route("/api/missions", get(requirements::missions))
        .route(
            "/api/requirements",
            get(requirements::list).post(requirements::create),
        )
        .route(
            "/api/requirements/{key}",
            get(requirements::get).patch(requirements::update),
        )
        .route(
            "/api/requirements/{key}/history",
            get(requirements::history),
        )
        .route(
            "/api/requirements/{key}/edge_cases",
            post(requirements::add_edge_case),
        )
        .route(
            "/api/requirements/{key}/examples",
            post(requirements::add_example),
        )
        .route(
            "/api/decisions",
            get(requirements::list_decisions).post(requirements::create_decision),
        )
        // changesets
        .route(
            "/api/changesets",
            get(changesets::list).post(changesets::open),
        )
        .route("/api/changesets/{key}", get(changesets::get))
        .route("/api/changesets/{key}/plan", post(changesets::plan))
        .route("/api/changesets/{key}/approve", post(changesets::approve))
        .route(
            "/api/changesets/{key}/versions",
            post(changesets::propose_version),
        )
        .route(
            "/api/changesets/{key}/edges",
            post(changesets::propose_edge),
        )
        .route(
            "/api/changesets/{key}/reviews",
            post(changesets::submit_review),
        )
        .route(
            "/api/changesets/{key}/transition",
            post(changesets::transition),
        )
        .route("/api/changesets/{key}/test", post(changesets::test))
        .route("/api/changesets/{key}/checks", get(changesets::checks))
        .route("/api/changesets/{key}/merge", post(changesets::merge))
        .route("/api/changesets/{key}/reject", post(changesets::reject))
        .route("/api/changesets/{key}/run", post(changesets::run))
        .route("/api/changesets/{key}/resolve", post(changesets::resolve))
        .route("/api/manual-edit", post(changesets::manual_edit))
        .route("/api/changesets/{key}/replan", post(changesets::replan))
        // agents
        .route("/api/agents", get(agents::status))
        .route("/api/chat", get(agents::history).post(agents::chat))
        .route("/api/chat/{key}/apply", post(agents::apply_chat))
        // tests & runs
        .route("/api/tests/run", post(runs::run_tests))
        .route("/api/tests/results", get(runs::test_results))
        .route("/api/run", post(runs::run_now))
        .route("/api/runs", get(runs::list_runs))
        // releases & environments
        .route("/api/releases", get(releases::list).post(releases::cut))
        .route("/api/releases/bisect", post(releases::bisect))
        .route("/api/releases/{key}", get(releases::get))
        .route("/api/releases/{a}/diff/{b}", get(releases::diff))
        .route("/api/environments", get(releases::environments))
        .route("/api/environments/{env}/promote", post(releases::promote))
        .route("/api/environments/{env}/rollback", post(releases::rollback))
        .route(
            "/api/environments/{env}/approvals",
            post(releases::approve_prod),
        )
        // embedded terminal
        .route("/api/terminal", get(terminal::ws))
        .route("/api/terminal/mcp-command", get(terminal::mcp_command))
        // settings
        .route("/api/settings", get(settings::get).put(settings::put))
        .route("/api/settings/agents", patch(settings::patch_agents))
}
