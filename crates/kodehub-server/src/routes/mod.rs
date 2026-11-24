//! HTTP routes. Every operation in doc 04's graph API has a route here; MCP tools map onto
//! these 1:1 via `kodehub_core::tools`.

pub mod agents;
pub mod c4;
pub mod changesets;
pub mod graph;
pub mod identity;
pub mod project_init;
pub mod queue;
pub mod releases;
pub mod requirements;
pub mod runs;
pub mod settings;
pub mod terminal;

use axum::routing::{delete, get, patch, post, put};
use axum::Router;

use crate::AppState;

pub fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(graph::health))
        .route("/api/health", get(graph::health))
        // accounts, orgs, projects, agent users, tokens, settings, credentials, audit
        .route("/api/auth/status", get(identity::status))
        .route("/api/auth/register", post(identity::register))
        .route("/api/auth/login", post(identity::login))
        .route("/api/auth/logout", post(identity::logout))
        .route("/api/me", get(identity::me))
        .route("/api/users", get(identity::users))
        .route(
            "/api/projects",
            get(identity::list_projects).post(identity::create_project),
        )
        .route("/api/projects/contexts", get(graph::common_contexts))
        .route(
            "/api/projects/{id}",
            patch(identity::patch_project).delete(identity::delete_project),
        )
        .route("/api/projects/{id}/open", post(identity::open_project))
        .route(
            "/api/projects/{id}/shares",
            get(identity::shares).put(identity::put_share),
        )
        .route(
            "/api/orgs",
            get(identity::list_orgs).post(identity::create_org),
        )
        .route("/api/orgs/{o}/members", get(identity::org_members))
        .route(
            "/api/orgs/{o}/members/{u}",
            put(identity::put_member).delete(identity::delete_member),
        )
        .route(
            "/api/agent-users",
            get(identity::list_agent_users).post(identity::create_agent_user),
        )
        .route(
            "/api/agent-users/{key}/disable",
            post(identity::disable_agent_user),
        )
        .route(
            "/api/tokens",
            get(identity::list_tokens).post(identity::create_token),
        )
        .route("/api/tokens/{id}", delete(identity::revoke_token))
        .route(
            "/api/scopes/{scope}/settings",
            get(identity::scope_settings),
        )
        .route(
            "/api/scopes/{scope}/settings/{key}",
            put(identity::put_setting).delete(identity::delete_setting),
        )
        .route(
            "/api/credentials",
            get(identity::list_credentials).post(identity::put_credential),
        )
        .route("/api/credentials/{id}", delete(identity::delete_credential))
        .route("/api/audit", get(identity::audit))
        // work queue, schedules, triggers, intents, spend
        .route("/api/work-items", get(queue::list).post(queue::create))
        .route("/api/work-items/{key}", get(queue::get))
        .route("/api/work-items/{key}/cancel", post(queue::cancel))
        .route("/api/queue/stats", get(queue::stats))
        .route(
            "/api/schedules",
            get(queue::schedules).post(queue::create_schedule),
        )
        .route("/api/schedules/replan", post(queue::replan))
        .route(
            "/api/schedules/{key}/enabled",
            post(queue::schedule_enabled),
        )
        .route("/api/test-now", post(queue::test_now))
        .route("/api/intents", get(queue::intents))
        .route("/api/spend", get(queue::spend))
        .route("/api/work-items/unblock", post(queue::unblock))
        .route("/api/approvals", get(queue::approvals))
        .route("/api/approvals/{key}/decide", post(queue::decide))
        .route("/api/notifications", get(queue::notifications))
        .route(
            "/api/notifications/{key}/read",
            post(queue::read_notification),
        )
        .route("/api/agent-types", get(queue::agent_types))
        .route("/api/agent-types/{name}", put(queue::put_agent_type))
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
            "/api/philosophy",
            get(c4::get_philosophy).put(c4::put_philosophy),
        )
        .route("/api/cascade", get(c4::cascade))
        .route("/api/requirements-by", get(c4::requirements_by))
        .route("/api/push-down", get(c4::push_down))
        .route("/api/lifecycle", get(c4::lifecycle))
        .route("/api/requirement-counts", get(c4::requirement_counts))
        .route("/api/node-versions", get(c4::versions))
        .route(
            "/api/use-cases",
            get(c4::list_use_cases).post(c4::create_use_case),
        )
        .route(
            "/api/use-cases/{key}",
            get(c4::get_use_case).patch(c4::patch_use_case),
        )
        .route("/api/use-cases/{key}/links", post(c4::link_use_case))
        .route("/api/use-cases/{key}/state", post(c4::set_use_case_state))
        .route("/api/test-container-types", get(c4::test_container_types))
        .route(
            "/api/test-container-types/{name}",
            put(c4::put_test_container_type),
        )
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
        .route("/api/releases/{key}/reselect", post(releases::reselect))
        .route("/api/releases/{key}/selection", get(releases::selection))
        .route("/api/releases/{key}/clone", post(releases::clone))
        .route("/api/releases/{key}/patch", post(releases::patch))
        .route("/api/releases/{key}/swap", post(releases::swap))
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
