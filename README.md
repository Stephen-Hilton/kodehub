# Kodehub — AI-native development (v0.1)

Code lives as nodes in a graph (ArangoDB) instead of text files. Humans set direction — the
philosophy, requirements, use cases and the high-level architecture; agents plan, write, review
and repair the code; deterministic containers build and test it. Everything runs on Kubernetes
(Kind on a laptop). The governing spec is `docs/Kodehub v0.1 Requirements Layered Spec for Claude
Code.md`; the build plan is `docs/09-v0.1-audit-and-plan.md`; every decision is in
`docs/decisions.md`; status is in `docs/STATUS.md`.

```
crates/
  kodehub-core       model, canonical JSON + BLAKE3 hashing, closures, signatures, tool catalog
  kodehub-store      ArangoDB store: identity, layered settings, credentials, SCD versions,
                     changesets, releases, work queue (claim and lease), schedules, intents, audit
  kodehub-render     graph → deterministic Rust crate / Python package (+ PyO3 extension)
  kodehub-runner     build cache, test bundles, container jobs (Kubernetes Jobs or local)
  kodehub-agents     changeset engine, agent types and actions, backends (Claude Code, API,
                     recorded, container), environment gates
  kodehub-server     API + orchestrator + agent dispatcher + remote MCP (OAuth) + the UI
  kodehub-scheduler  thin scheduler container (timers and triggers)
  kodehub-storage    storage service: S3-style objects, local / cloud / hybrid, project backups
  kodehub-worker     deterministic containers: build, test, git import/export, transcribe, agent
  kodehub-cli        `kodehub` CLI and `kodehub mcp` (stdio MCP over the same API)
app/                 React + TypeScript + Cytoscape UI, Tauri v2 desktop shell
deploy/              Kind cluster, Kubernetes manifests, images
examples/auth-poc    seed project (doc 08)
```

## Prerequisites

- Docker (ArangoDB 3.12 via `docker compose`)
- Rust stable, Python ≥ 3.11 with `uv` (or `venv` + pip), Node ≥ 20 (UI build only)
- The `claude` CLI for agents (Claude Code backend). No API key is needed.

## Run it

Everything in Kind (server + UI on http://127.0.0.1:17878):

```sh
cd app && npm install && cd ..   # once
make up                          # Kind cluster, images, ArangoDB, storage, server, schedulers
```

Sign in (the first account administers the installation), create a project (or load the auth
example on an empty map), and watch the Work screen. Agents in the cluster need a credential in
Settings → Credentials: an Anthropic API key, or — for a personal project you own — a Claude
subscription token. Without one, agent work is blocked and the Work screen says so.

Fast dev loop (ArangoDB in Kind, server on the host):

```sh
cd app && npm install && cd ..   # once
make dev                         # ArangoDB + kodehub-server on http://127.0.0.1:7878 (serves the UI)
make seed                        # in another terminal: import examples/auth-poc
open http://127.0.0.1:7878       # or `make desktop` for the Tauri app
```

Everything Kodehub writes (config, build cache, renders, venv) lives in `./.kode`.

### Agents and your Claude login

The default agent backend drives the official `claude` CLI headless with your own login, so a
Pro/Max subscription works. To pick a long-lived token per task instead, keep tokens in env vars
named like `DEV1_TOKEN` in a file **outside** the repo and point to it from a git-ignored
`local.mk`:

```make
KODEHUB_ENV_FILE = $(HOME)/path/to/tokens.env
```

The server never reads the file. `scripts/claude-as.sh` sources it in each agent's own process,
and the status-bar selector stores only the variable *name* (decisions D-021, D-027).
`ANTHROPIC_API_KEY` is never read and is stripped from every child process. The Messages API
backend uses `KODEHUB_ANTHROPIC_API_KEY`. The **recorded** backend replays the doc 08 demo
deterministically with no credentials.

## The demo (doc 08)

Set the project's *How engaged do you want to be?* to `plans` to approve each plan yourself,
as in the v0.05 demo (by default agents proceed and you approve only requirement changes, new
contexts and production).

1. Everything is green (Requirements → **Tests**).
2. Click **+ Edge case** on *R1 — Generic failure* and add "A locked account must also return
   `invalid_credentials`".
3. R1 and everything beneath it turns amber. The orchestrator opens a changeset, and ghost nodes
   show the plan.
4. **Approve plan.** The tester writes a test from the requirement and the coder changes `User`
   and `verify_credentials`. The **blind** reviewer flags the timing side channel, the coder
   fixes it, and a Decision is recorded. Tests pass, the changeset merges, and the map goes green.
5. Search `verify_credentials` and **Run now** with a locked user: `invalid_credentials`, from
   the cache.
6. Open **Releases**, create a release and promote it to `test`.

## Tests

```sh
make check      # fmt, clippy, unit + integration tests (ArangoDB in Kind), UI typecheck + unit,
                # Playwright, and the cluster check (images, deploy, end-to-end smoke)
make ui-test    # Playwright only
make k8s-check  # images + deploy + smoke only
make live-test  # real Claude Code agents (M4 MCP + M5 demo); token chosen by env var NAME
```

v0.1 acceptance tests: `kodehub-store/tests/s2_identity.rs`, `s6_queue.rs`;
`kodehub-server/tests/s2_http_auth.rs`, `s3_c4_model.rs`, `m7_releases.rs` (S4),
`s7_containers.rs`, `s8_agents.rs`, `s8_agent_containers.rs`, `s9_git.rs`, `s11_oauth_mcp.rs`;
`kodehub-storage/tests/s5_storage.rs`; `kodehub-scheduler/tests/s6_scheduler.rs`;
`kodehub-runner/tests/s7_bundles.rs`; `app/e2e/demo.spec.ts`, `app/e2e/v01.spec.ts`.

| Milestone | Acceptance tests |
| --- | --- |
| M0 | `/health` (server + DB), `make check` |
| M1 | `crates/kodehub-store/tests/m1_graph_core.rs`, `crates/kodehub-core/tests/golden_hash.rs` |
| M2 | `crates/kodehub-runner/tests/m2_render_build_run.rs` |
| M3 | `crates/kodehub-runner/tests/m3_tests_traceability.rs` |
| M4 | `crates/kodehub-server/tests/m4_http_api.rs`, `m4_claude_mcp.rs` (live) |
| M5 | `crates/kodehub-server/tests/m5_agents_demo.rs` (recorded), `m5_live_claude_code.rs` (live), `crates/kodehub-agents/tests/api_backend.rs` |
| M6 | `app/e2e/demo.spec.ts` (Playwright), `app/src/lib/*.test.ts` |
| M7 | `crates/kodehub-server/tests/m7_releases.rs` |
| M8 | `crates/kodehub-runner/tests/m8_pyo3_binding.rs` |

## Use Kodehub from Claude Code

Remote MCP (OAuth — the assistant becomes its own agent user acting for you):

```sh
claude mcp add --transport http kodehub http://127.0.0.1:17878/mcp
```

Local (stdio) with a token from Settings → External agents:

```sh
KODEHUB_TOKEN=khp_… claude mcp add kodehub -- $(pwd)/target/debug/kodehub mcp
```

The embedded terminal offers to do this for you as soon as you run `claude` inside it.
