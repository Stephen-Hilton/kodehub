# Kodehub v0.1 — build status

_Last updated by the build agent on 2026-11-25._

`make check` is green: fmt, clippy `-D warnings`, unit and integration tests against ArangoDB in
a local Kind cluster, UI typecheck and unit tests, the Playwright end-to-end suite, and the
cluster check (image builds, deploy, smoke: health, storage round trip, an end-to-end *Test now*
through scheduler → orchestrator → build Job → test Jobs, an agent task in its own container,
and speech transcribed by Whisper in the cluster), and the storage service's S3 path against
SeaweedFS in the cluster.

The plan and acceptance criteria are in `docs/09-v0.1-audit-and-plan.md`; every decision is in
`docs/decisions.md` (D-041 … D-054 for v0.1).

**Ontology v2** (`docs/ontology.md`, plan `docs/10-ontology-plan.md`, D-055 … D-063) is in
place: shared collections, project-version trees, proposals on work items, project versions as
releases with deployments, requirement versions with tags, per-project agent types. The Kind
database was wiped and re-created by the server (74 collections, one named graph `kodehub`).
106 Rust tests, 15 UI unit tests and both Playwright specs pass.

### Manual pass on the deployed cluster (2026-11-25)

Driven through the deployed API at `127.0.0.1:17878` with a human session (the same requests the
app makes), every step checked:

- sign up; a new project has one open, current project version; seed the auth example (22
  code/test nodes, map levels and `tested_with` / `calls` edges, two tagged requirements, the
  decision as a requirement of kind `decision`, eight agent types with permissions, invariants)
- all 9 tests pass, coverage green
- edit R1 (new requirement version) → the orchestrator plans a proposal on a work item (ghost
  node on the map) → approve → tests first, code, blind review with rework, decision recorded,
  accepted (4 versions) on recorded responses in agent containers → work item done, coverage
  green
- deploy r1 DEV → TEST (r1 frozen) → QA → PRE-PROD → PROD (waits for and gets a human
  approval); run in PROD uses r1
- hotfix of r1 through a proposal driven by hand; it differs from r1 only in
  `verify_password`; deployed straight into PROD with approval; the next release stays current
  and open
- rollback of PROD to r1 (waits for approval, then runs r1)
- backup and restore as a new project (same code, invariants hold)
- timeline stops, the map of an earlier project version, invariants
- a remote agent may add a work item but may not deploy or accept a proposal

Git export in the cluster was not exercised by hand (it needs a repository reachable from the
cluster); `s9_git` covers import, export round trip and restore.

| Step | Status | Evidence |
| --- | --- | --- |
| S1 Audit and plan | ✅ | `docs/09-v0.1-audit-and-plan.md` |
| S2 Identity, tenancy, settings, credentials, audit | ✅ | `kodehub-store/tests/s2_identity.rs`, `kodehub-server/tests/s2_http_auth.rs` |
| S3 SCD versions at every C4 level, philosophy, requirements, use cases, lifecycle | ✅ | `kodehub-server/tests/s3_c4_model.rs`, `m1_graph_core.rs` |
| S4 Project versions and deployments (open/frozen, hotfix, five environments, rollback, bisect) | ✅ | `kodehub-server/tests/m7_deployments.rs`, `kodehub-store/tests/o5_deployments.rs` |
| S5 Kind, images, storage service (local / cloud / hybrid, per-project backups) | ✅ | `kodehub-storage/tests/s5_storage.rs`, `scripts/k8s.sh smoke` |
| SeaweedFS as the optional S3 cold tier (D-054) | ✅ | `kodehub-storage/tests/s5_seaweedfs.rs` via `scripts/k8s.sh seaweed-check` |
| S6 Claim and lease, work queue, scheduler, intents and negotiation | ✅ | `kodehub-store/tests/s6_queue.rs`, `kodehub-scheduler/tests/s6_scheduler.rs` |
| S7 Build and test containers, shell test bundles, formal runs, baseline | ✅ | `kodehub-runner/tests/s7_bundles.rs`, `kodehub-server/tests/s7_containers.rs`, in-cluster smoke |
| S8 Agents: types, dispatcher, kickoff chain, tests before code, escalation, blockers, budgets, agent containers | ✅ | `kodehub-server/tests/s8_agents.rs`, `s8_agent_containers.rs`, `m5_*` |
| S9 Git import (import agent mapping) and export, graph backups | ✅ | `kodehub-server/tests/s9_git.rs` |
| S10 UI: screens, in-node badges and overlays | ✅ | `app/e2e/demo.spec.ts`, `app/e2e/v01.spec.ts` |
| S11 Speech transcription, remote MCP over OAuth 2.1 | ✅ | `kodehub-server/tests/s11_oauth_mcp.rs`, in-cluster smoke |
| §11.4–11.5 Sysadmin timer, test budget → cadence | ✅ | `kodehub-server/tests/s11_sysadmin_budget.rs` |

## Not done / known limits

- Live agents in the cluster need a stored credential (an API key, or a personal subscription
  token on a user-owned project). CI-style runs use the recorded backend.
- The sysadmin check reads this machine's CPUs and load (`/proc/loadavg`), not per-node
  Kubernetes metrics, and cannot yet move work between machines.
- Opaque (non-Rust/Python) imported code stays staged, not decomposed.
- Hash-only referencing inside code bodies, multi-cluster deployment and IDE-grade editing remain
  out of scope.
- `docker-compose.yml` is obsolete (everything runs in Kind) but still in the tree.
