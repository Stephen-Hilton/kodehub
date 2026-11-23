# 07 — Milestones and acceptance criteria

Build in order. Each milestone is done only when every criterion passes and `make check` is green.
Write automated tests for each criterion where possible.

## M0 — Foundations

- Cargo workspace with the crates in doc 01; `app/` scaffolded with Tauri v2 + React + TS.
- `docker-compose.yml` starts ArangoDB; `make dev` starts DB + server.
- `make check` runs fmt, clippy (deny warnings), unit tests, integration tests.

**Accept:** fresh clone → `make dev` → `GET /health` returns OK and reports DB connectivity.

## M1 — Graph core

- Schema bootstrap creates all collections, edge collections, the named graph and indexes.
- Canonicalisation and `content_hash` / `closure_hash` in `kodehub-core`.
- Symbol/version writes with the versioning rule (doc 02), in a transaction.
- Invariant checks: exactly one `current_version` per symbol.
- `kodehub seed examples/auth-poc` imports the YAML dataset (doc 08).

**Accept:**
- Hashes are identical across runs and machines (golden-file test).
- Re-importing unchanged seed creates zero new versions.
- Changing one body creates exactly one new version and flips edges correctly.
- Changing a dependency changes the closure hash of every dependent and nothing else.
- Attempting to create a second `current_version` edge fails.

## M2 — Render, build, run

- Renderer for Rust and Python (doc 03), deterministic.
- Build cache keyed by closure hash and target.
- Run now via CLI: `kodehub run <symbol> --args '{...}' --env dev`.

**Accept:**
- Rendering the same closure twice gives byte-identical output.
- Seed project compiles (Rust) and imports (Python).
- Second run of an unchanged Rust function performs no compile and returns in under 1 second.
- Changing a helper triggers rebuild of its dependents only.
- Timeout kills runaway functions after 10 seconds.

## M3 — Tests and traceability

- Test rendering and execution for both languages; results stored per closure hash.
- AQL queries: `coverage`, `orphans`, `impact`, `duplicates`.
- Requirement anatomy with revision history.

**Accept (on seed data):**
- Coverage reports every requirement with pass/fail.
- Adding a symbol nothing reaches makes it appear in `orphans`.
- `impact(hash_password)` returns the Secure Authentication group and both requirements.
- An intentionally broken helper turns its requirements red via propagation.
- Two symbols with identical bodies appear in `duplicates`.

## M4 — Server API and MCP

- All operations from doc 04 over HTTP; WebSocket events (doc 01).
- MCP server exposing the same operations.

**Accept:** an integration test drives the full seed scenario purely through HTTP; Claude Code
connected via MCP can list requirements and run coverage.

## M5 — Agents and changesets

- Orchestrator, coder, reviewer, tester with prompts in `prompts/`.
- Changeset lifecycle (doc 04), including blind review isolation and Decision logging.
- Requirement edits mark dependants at risk and auto-open a changeset.

**Accept:** the demo scenario in doc 08 runs end to end: the human adds an edge case; agents
update code and tests; the review is blind (assert the reviewer's prompt contains none of the
coder's messages); a Decision node is recorded; merge happens only after tests pass; coverage is
green afterwards. Provide a deterministic mode with recorded LLM responses for CI.
- The demo passes on the **Claude Code backend** using only a developer's Claude login (no API
  key set anywhere) and on the **API backend** with `KODEHUB_ANTHROPIC_API_KEY`.

## M6 — Desktop interface

- Layout, mission banner, map with semantic zoom and health colours, sidebars, node panel,
  run now with generated forms, save as example, ghost nodes, time slider, embedded terminal.

**Accept:** a person can perform the doc 08 demo entirely in the app without the CLI.

## M7 — Releases and environments

- Cut release, diff, promote with gates, rollback, simulated QA and prod.

**Accept:**
- Release manifest is immutable (write attempts fail).
- Promoting dev → test → qa → prod leaves exactly one `currently_running` edge per environment.
- Rollback restores the previous release.
- Diff between two releases lists exactly the changed symbols.

## M8 (stretch) — Rust core with Python binding

- `binds_to` edges generate a PyO3 wrapper and a maturin build target (`python-ext`).

**Accept:** Python wrapper `login()` calls the Rust `verify_credentials` through the generated
extension; changing the Rust signature marks the Python wrapper at risk.
