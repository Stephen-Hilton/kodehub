# Kodehub — build instructions for Claude Code

Kodehub (working name, rename freely) is an AI-native development environment. Code is stored as
nodes in a graph database instead of text files. Humans set direction (philosophy, requirements,
use cases, high-level architecture); AI agents write, review, test and run the code.

The current target is **v0.1**, layered on top of the working v0.05 pilot.

## Read first, in this order

1. `docs/Kodehub v0.1 Requirements Layered Spec for Claude Code.md` — **the governing spec.**
   Where any other document conflicts with it, the spec wins. Where it is silent, keep existing
   behaviour.
2. `docs/09-v0.1-audit-and-plan.md` — the audit of v0.05 against the spec and the step plan,
   with acceptance criteria. **Work from this.** (Written in rollout step 1.)
3. `docs/decisions.md` — every build-time decision, newest first.
4. `docs/00-vision.md` … `docs/08-poc-dataset.md` — the v0.05 design. Still useful background;
   superseded wherever it conflicts with the v0.1 spec.

## How to work

- Build plan step by step, in order. Do not start a step until the previous one meets every
  acceptance criterion and `make check` passes.
- `make check` = format, lint, unit tests, and integration tests against a local Kind cluster.
- Layer, don't rewrite: extend and refactor working components incrementally; keep the system
  running at each step.
- Preserve the existing graph rendering in the UI (how it looks and draws nodes).
- Prefer the simplest thing that satisfies the acceptance criteria.
- When the spec is ambiguous, choose, then record the choice in `docs/decisions.md` (context,
  decision, consequence). Do not stop to ask unless a choice is irreversible.
- When the spec is wrong or impossible, say so in `docs/decisions.md` and propose the fix.
- The API is the only way anything (UI, MCP, CLI, agents, containers) reads or writes the graph.
  The orchestrator owns all data access behind it.

## Hard rules (invariants — enforce in code, test in code)

1. Every node (code, component, container, context) has **at most one stable version**: one
   version edge with `validFrom` set and `validTo` empty, none while only planned. A node's version
   validity intervals never overlap. Release membership is separate (`supports` edges).
2. Version nodes are **immutable**. A change always creates a new version node. Build artifacts
   live in their own nodes keyed by `closure_hash`, never on the version.
3. A version's `content_hash` is deterministic: same canonical content → same hash, on any machine.
4. Releases are **immutable** once cut (first promoted out of DEV). A fix after DEV is a patch
   release (a clone at the same environment), never an edit of a cut release.
5. A release is in **at most one** environment at a time, and is cloned only into its own
   environment or a lower one. PRE-PROD and PROD (any environment with
   `max_releases = 1`) hold **at most one** release.
6. A changeset promotes **all or nothing**, and cannot promote if it introduces orphan code, fails
   any affected formal test, or — when the project's `blind_review` setting is on (the default) —
   lacks a passing blind review.
7. No LLM call happens while a test runs. Test, build and import containers are deterministic.
8. Every API and MCP call is authenticated and scoped to the caller's permissions. No anonymous
   access.
9. Every graph node belongs to exactly one project.

## Credentials (hard rules)

- Never read or set `ANTHROPIC_API_KEY` anywhere in this repo (code, `.env`, scripts, tests,
  containers). If it is present, Claude Code bills API credits instead of the developer's
  subscription. Kodehub passes API keys to agent containers under its own variable name.
- Model and Git credentials are layered settings, stored encrypted, never logged, never written to
  the graph in plain text, and handed to agent containers only for the duration of their work.
- **Subscription tokens** are accepted only at the user level, for projects that user owns
  directly. Organizations and org-owned projects accept API keys only.
- A subscription OAuth token is only ever given to the official `claude` CLI. It is **never** sent
  to the Messages API.
- Personal access tokens are stored only as hashes and shown once.
- Tests must pass in recorded-response mode with no model credential at all.

## Pilot simplifications (deliberate)

- Display names are unique per language within a project (enforced on write). Hash-only
  referencing inside code bodies is a later phase.
- Environments are Kubernetes namespaces in the same (Kind) cluster; QA, pre-prod and prod are not
  real infrastructure.
- Authentication is local accounts + session tokens + personal access tokens. OAuth for remote
  MCP and OIDC come later.
- No migration of v0.05 data: start a fresh database and re-create the seed project.

## Non-goals for v0.1

Cloud deployment beyond a single cluster, code-editing comparable to an IDE, decomposing languages
other than Rust and Python (others import as opaque modules), Git reverse sync and re-import,
performance at large scale.
