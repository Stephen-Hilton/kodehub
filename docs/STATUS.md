# Kodehub pilot — build status

_Last updated by the build agent on 2026-11-05._

`make check` is green: fmt, clippy `-D warnings`, unit and integration tests against a real
ArangoDB / cargo / pytest, UI typecheck and unit tests, and the Playwright end-to-end demo.

## Milestones

| | Status | Evidence |
| --- | --- | --- |
| **M0** Foundations | ✅ | Workspace + `app/` (Tauri v2 / React / TS). `make dev` → `GET /health` reports DB connectivity. `make check`. |
| **M1** Graph core | ✅ | `kodehub-store/tests/m1_graph_core.rs`: golden content hashes, idempotent re-seed, one body edit makes exactly one version, a dependency change alters exactly the dependents' closure hashes, a second `current_version` edge is refused (unique index). |
| **M2** Render/build/run | ✅ | `kodehub-runner/tests/m2_render_build_run.rs`: byte-identical renders, the seed compiles (Rust) and imports (Python) incl. namespaces, the second run is a cache hit in < 1 s, only dependents rebuild, the 10 s timeout kills the process group. |
| **M3** Tests & traceability | ✅ | `m3_tests_traceability.rs`: coverage per requirement, orphans, `impact(hash_password)` = group + R1 + R2, a broken helper turns requirements red via propagation, exact and near duplicates. |
| **M4** API & MCP | ✅ | `kodehub-server/tests/m4_http_api.rs` drives the seed scenario purely over HTTP (+ WebSocket events, MCP over stdio). `m4_claude_mcp.rs`: real Claude Code lists requirements and runs coverage over MCP. |
| **M5** Agents & changesets | ✅ / ⚠️ | Recorded: `m5_agents_demo.rs` (blind review asserted, Decision recorded, merge only after tests, coverage green, cache-hit run now). **Claude Code backend, live:** `m5_live_claude_code.rs` passes on a subscription token with no API key (about 2 min). **API backend:** mock-tested only, because no `KODEHUB_ANTHROPIC_API_KEY` was available (D-030). |
| **M6** Desktop UI | ✅ | `app/e2e/demo.spec.ts`: the entire doc 08 demo performed in the UI (Playwright), screenshots in `.kode/shots/e2e/`. Tauri bundle builds (`make desktop-build`). |
| **M7** Releases & environments | ✅ | `m7_releases.rs`: immutable manifests, gated dev → test → qa → prod with exactly one `currently_running` per environment, one-time human approval for prod, rollback walks back, diffs list exactly the changed symbols (with reasons), bisect. |
| **M8** (stretch) PyO3 binding | ✅ | `m8_pyo3_binding.rs`: Python `login()` calls Rust `verify_credentials` through a generated extension (asserted in the render, cached), and a Rust signature change marks the wrapper (and R1) at risk. Each extension builds in its own target dir. |

## Critical reviews (headless Claude Code agents)

| Round | Scope | Outcome |
| --- | --- | --- |
| `docs/reviews/review-m4.md` | M0–M4 | 2 critical, 6 high, 12 medium. All but a few pilot-scope items fixed (D-027). |
| `docs/reviews/review-m6.md` | M5–M7 + UI (UI 6.5/10) | 22 findings, all accepted and fixed (D-036, D-037). |
| `docs/reviews/review-final.md` | Completion + verification (UI 7.5/10; M5/M8 "partial") | 15 findings: per-launch UI secret for human-only actions, no code/trace edits outside changesets, requirement-centric `covered_revision`, PyO3 per-closure target dirs, demo ends green, and more. All fixed (D-039). |

## Guardrails worth knowing

- Code changes only through changesets. A merge needs passing affected tests, an attested blind
  review (engine reviewer, or a human with the UI secret), no new orphans, and a verifying test
  for every edited requirement.
- Human-only actions (approve, requirement edits, prod approval, chat Apply, terminal…) need the
  per-launch UI secret. Agents over MCP/HTTP can't perform them.
- Generated code runs with a cleared environment in a no-network sandbox that can't read
  credentials or the UI secret.

## Open items

- Run the M5 demo live on the **API backend** (needs a Console key):
  `KODEHUB_LIVE_DEMO=1 KODEHUB_LIVE_BACKEND=api make live-test`.
- Exact pinning / lockfiles for third-party crates and packages (D-027 "not done").
- Doc 02's merge guardrail (split merged functions whose option branches diverge) is a later
  phase.

## How to look at it

```sh
make dev            # server + UI on http://127.0.0.1:7878
make seed           # import the demo project
make desktop        # Tauri window (with make dev running)
```
