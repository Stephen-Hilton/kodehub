# 01 — Architecture

## Shape

One product, two deployment modes, same API:

```
┌──────────────────────┐        HTTP/JSON + WebSocket        ┌───────────────────────────┐
│ Desktop client       │ ──────────────────────────────────▶ │ Kodehub server (Rust)     │
│ Tauri v2 + React/TS  │                                     │ - graph API               │
└──────────────────────┘                                     │ - render / build / run    │
┌──────────────────────┐        MCP (stdio or HTTP)          │ - agent runtime           │
│ External agents      │ ──────────────────────────────────▶ │ - environment manager     │
│ (e.g. Claude Code)   │                                     └────────────┬──────────────┘
└──────────────────────┘                                                  │
┌──────────────────────┐                                     ┌────────────▼──────────────┐
│ kodehub CLI          │ ──── same HTTP API ───────────────▶ │ ArangoDB (graph store)    │
└──────────────────────┘                                     └───────────────────────────┘
```

- **Solo / offline:** server, database and client all run on the developer's machine. (Pilot.)
- **Teams (later):** one central server; laptops keep a local replica that syncs. Version nodes
  are immutable, so sync is mostly copying; conflicts occur only when two people move the same
  `current_version` edge.

## Tech stack

| Concern | Choice | Notes |
| --- | --- | --- |
| Server | Rust, `axum`, `tokio` | Single binary `kodehub-server`. |
| Graph store | ArangoDB (Docker, community edition) | Document + edge collections, one named graph. Talk to it via its HTTP API using a thin `reqwest` wrapper rather than depending on a third-party driver. |
| Hashing | BLAKE3 | Over canonical JSON (see data model). |
| Desktop client | Tauri v2, React, TypeScript, Vite | |
| Graph rendering | Cytoscape.js | Compound nodes give C4 semantic zoom. |
| Agent runtime | Pluggable backend (see doc 04): **Claude Code backend** (default) drives the official `claude` CLI headless, using the developer's own Claude login, so a Pro/Max subscription works; **API backend** calls the Messages API with `KODEHUB_ANTHROPIC_API_KEY` | Model configurable. Never send a subscription OAuth token to the Messages API directly. Never use the env var name `ANTHROPIC_API_KEY`. |
| Embedded terminal | `xterm.js` + a PTY on the server | So the developer can run interactive Claude Code inside Kodehub, as in VS Code. |
| MCP | Expose graph API as an MCP server | Lets Claude Code itself operate on the graph. |
| Rust toolchain | stable `cargo` on the host | |
| Python toolchain | Python ≥ 3.11, `pytest` | |

## Repo layout

```
crates/
  kodehub-core/     model types, canonicalisation, hashing, invariants
  kodehub-store/    ArangoDB wrapper, schema bootstrap, AQL queries
  kodehub-render/   graph → temporary Rust/Python projects
  kodehub-runner/   build cache, compile, test, run-now, sandbox
  kodehub-agents/   orchestrator, coder, reviewer, tester, environment agents
  kodehub-server/   axum HTTP API, WebSocket events, MCP server
  kodehub-cli/      `kodehub` command-line client
app/                Tauri desktop client
examples/auth-poc/  seed dataset (YAML) — see docs/08
docker-compose.yml  ArangoDB
Makefile            `make check`, `make dev`, `make seed`
docs/decisions.md   decision log written during the build
```

## Data directories

`~/.kode/` holds config, the build cache (`cache/<closure_hash>/`) and temp render dirs.
Everything else lives in ArangoDB.

## Events

The server pushes events over WebSocket so the UI updates live: `node.changed`,
`changeset.updated`, `test.result`, `run.output`, `agent.activity`, `env.deployed`.
