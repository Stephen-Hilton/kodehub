//! HTTP routes. Every operation in doc 04's graph API has a route here; MCP tools map onto
//! these 1:1 via `kodehub_core::tools`.

pub mod agents;
pub mod c4;
pub mod deploy;
pub mod graph;
pub mod identity;
pub mod oauth;
pub mod project_init;
pub mod proposals;
pub mod queue;
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
        // remote MCP behind OAuth 2.1 (spec §13A.2)
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth::protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth::protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth::authorization_server),
        )
        .route("/oauth/register", post(oauth::register))
        .route(
            "/oauth/authorize",
            get(oauth::authorize_page).post(oauth::authorize_submit),
        )
        .route("/oauth/token", post(oauth::token))
        .route("/mcp", post(oauth::mcp))
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
        .route("/api/git/import", post(queue::git_import))
        .route("/api/git/export", post(queue::git_export))
        .route("/api/backups", post(queue::backup))
        .route(
            "/api/transcribe",
            post(queue::transcribe).layer(axum::extract::DefaultBodyLimit::max(26 * 1024 * 1024)),
        )
        .route("/api/projects/restore", post(queue::restore))
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
        .route("/api/node-tests", get(c4::node_tests))
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
        // work items carry their proposals (D-058)
        .route("/api/proposals", get(proposals::list).post(proposals::open))
        .route(
            "/api/work-items/{key}/proposal",
            get(proposals::get).post(proposals::open_on),
        )
        .route("/api/work-items/{key}/proposal/plan", post(proposals::plan))
        .route(
            "/api/work-items/{key}/proposal/approve",
            post(proposals::approve),
        )
        .route(
            "/api/work-items/{key}/proposal/versions",
            post(proposals::propose_version),
        )
        .route(
            "/api/work-items/{key}/proposal/edges",
            post(proposals::propose_edge),
        )
        .route(
            "/api/work-items/{key}/proposal/reviews",
            post(proposals::submit_review),
        )
        .route(
            "/api/work-items/{key}/proposal/transition",
            post(proposals::transition),
        )
        .route("/api/work-items/{key}/proposal/test", post(proposals::test))
        .route(
            "/api/work-items/{key}/proposal/checks",
            get(proposals::checks),
        )
        .route(
            "/api/work-items/{key}/proposal/accept",
            post(proposals::accept),
        )
        .route(
            "/api/work-items/{key}/proposal/reject",
            post(proposals::reject),
        )
        .route("/api/work-items/{key}/proposal/run", post(proposals::run))
        .route(
            "/api/work-items/{key}/proposal/resolve",
            post(proposals::resolve),
        )
        .route(
            "/api/work-items/{key}/proposal/replan",
            post(proposals::replan),
        )
        .route("/api/manual-edit", post(proposals::manual_edit))
        // agents
        .route("/api/agents", get(agents::status))
        .route("/api/chat", get(agents::history).post(agents::chat))
        .route("/api/chat/{key}/apply", post(agents::apply_chat))
        // tests & runs
        .route("/api/tests/run", post(runs::run_tests))
        .route("/api/tests/results", get(runs::test_results))
        .route("/api/run", post(runs::run_now))
        .route("/api/runs", get(runs::list_runs))
        // project versions are the releases; deployments carry them into environments
        .route(
            "/api/project-versions",
            get(deploy::list_versions).post(deploy::create_version),
        )
        .route("/api/project-versions/bisect", post(deploy::bisect))
        .route("/api/project-versions/{key}", get(deploy::get_version))
        .route(
            "/api/project-versions/{key}/freeze",
            post(deploy::freeze_version),
        )
        .route(
            "/api/project-versions/{key}/current",
            post(deploy::make_current),
        )
        .route("/api/project-versions/{key}/hotfix", post(deploy::hotfix))
        .route("/api/project-versions/{key}/trace", get(deploy::trace))
        .route("/api/project-versions/{a}/diff/{b}", get(deploy::diff))
        .route("/api/environments", get(deploy::environments))
        .route("/api/environments/{env}/rollback", post(deploy::rollback))
        .route(
            "/api/deployments",
            get(deploy::list_deployments).post(deploy::deploy),
        )
        .route("/api/deployments/{key}", get(deploy::get_deployment))
        .route("/api/deployments/{key}/approve", post(deploy::approve))
        .route("/api/deployments/{key}/retry", post(deploy::retry))
        // embedded terminal
        .route("/api/terminal", get(terminal::ws))
        .route("/api/terminal/mcp-command", get(terminal::mcp_command))
        // settings
        .route("/api/settings", get(settings::get).put(settings::put))
        .route("/api/settings/agents", patch(settings::patch_agents))
}
