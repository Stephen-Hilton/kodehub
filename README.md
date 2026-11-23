# Kodehub — AI-native development pilot

Code lives as nodes in a graph (ArangoDB) instead of text files. Humans define requirements;
agents write, review, test and run the code, and every change traces back to a requirement. See
`docs/00-vision.md` for the why and `docs/decisions.md` for every build-time decision.

```
crates/
  kodehub-core     model, canonical JSON + BLAKE3 hashing, closures, signatures, tool catalog
  kodehub-store    ArangoDB (HTTP) store: versioning, changesets, releases, AQL traceability
  kodehub-render   graph → deterministic Rust crate / Python package (+ PyO3 extension)
  kodehub-runner   build cache, sandboxed compile/test/run-now, Python venv
  kodehub-agents   orchestrator / coder / blind reviewer / tester engine, environment gates,
                   Claude Code · Messages API · recorded backends
  kodehub-server   axum HTTP API + WebSocket events + PTY terminal (serves the UI)
  kodehub-cli      `kodehub` CLI and `kodehub mcp` (MCP server over the same API)
app/               React + TypeScript + Cytoscape + xterm UI, Tauri v2 desktop shell
examples/auth-poc  seed project (doc 08)
```

## Prerequisites

- Docker (ArangoDB 3.12 via `docker compose`)
- Rust stable, Python ≥ 3.11 with `uv` (or `venv` + pip), Node ≥ 20 (UI build only)
- The `claude` CLI for agents (Claude Code backend). No API key is needed.

## Run it

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

1. Everything is green (run **Tests** in the left sidebar).
2. Click **+ Edge case** on *R1 — Generic failure* and add "A locked account must also return
   `invalid_credentials`".
3. R1 and everything beneath it turns amber. The orchestrator opens a changeset, and ghost nodes
   show the plan.
4. **Approve plan.** The tester writes a test from the requirement and the coder changes `User`
   and `verify_credentials`. The **blind** reviewer flags the timing side channel, the coder
   fixes it, and a Decision is recorded. Tests pass, the changeset merges, and the map goes green.
5. Search `verify_credentials` and **Run now** with a locked user: `invalid_credentials`, from
   the cache.
6. Open **Releases**, cut a release and promote it to `test`.

## Tests

```sh
make check      # fmt, clippy -D warnings, unit + integration tests (real ArangoDB, cargo, pytest), UI typecheck + unit
make ui-test    # Playwright: performs the whole demo in the UI
make live-test  # real Claude Code agents (M4 MCP + M5 demo); token chosen by env var NAME
```

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

```sh
claude mcp add kodehub -- $(pwd)/target/debug/kodehub mcp
```

The embedded terminal offers to do this for you as soon as you run `claude` inside it.
