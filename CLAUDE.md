# Kodehub — pilot build instructions for Claude Code

Kodehub (working name, rename freely) is an AI-native development environment. Code is stored as
nodes in a graph database instead of text files. Humans define requirements; AI agents write,
review, test and run the code. This repo is a **pilot** to see what that looks like in practice.

## Read first, in this order

1. `docs/00-vision.md` — why this exists. Short. Read it so your design choices match the intent.
2. `docs/01-architecture.md` — components, tech stack, repo layout.
3. `docs/02-data-model.md` — the ontology. The single most important document.
4. `docs/03-render-build-run.md` — turning graph nodes into runnable Rust/Python.
5. `docs/04-agents-and-changesets.md` — agent roles and how changes flow.
6. `docs/05-interface.md` — desktop UI.
7. `docs/06-releases-environments.md` — releases, environments, promotion.
8. `docs/07-milestones.md` — the build plan with acceptance criteria. **Work from this.**
9. `docs/08-poc-dataset.md` — the seed project used to prove everything works.

## How to work

- Build milestone by milestone, in order. Do not start a milestone until the previous one meets
  every acceptance criterion.
- Every milestone ends with `make check` passing (format, lint, unit tests, integration tests).
- Prefer the simplest thing that satisfies the acceptance criteria. This is a pilot.
- When the spec is ambiguous, choose, then record the choice in `docs/decisions.md` as a short
  entry (context, decision, consequence). Do not stop to ask unless a choice is irreversible.
- When the spec is wrong or impossible, say so in `docs/decisions.md` and propose the fix.
- Keep the graph API the only way anything (UI, CLI, agents) reads or writes the graph.

## Hard rules (invariants — enforce in code, test in code)

1. Every symbol has **exactly one** `current_version` edge.
2. Version nodes are **immutable**. A change always creates a new version node.
3. A version's `content_hash` is deterministic: same canonical content → same hash, on any machine.
4. Release nodes are **immutable** once cut.
5. Each environment has **exactly one** `currently_running` edge (or none before first deploy).
6. A changeset cannot merge if it introduces orphan code, fails any affected test, or lacks a
   passing blind review.

## Pilot simplifications (deliberate — do not "fix" these)

- Single user, local only. No auth, no multi-user sync.
- Display names are unique per language within a project (enforced on write). Hash-only
  referencing inside code bodies is a later phase.
- Rendering produces one generated module per language per build (topologically ordered).
- Sandboxing is a temp directory + subprocess + timeout. Containers/WASM come later.
- QA and Prod environments are simulated (local processes), not real infrastructure.

## Non-goals for the pilot

Multi-user collaboration, cloud deployment, languages other than Rust and Python, a code-editing
experience comparable to an IDE, performance at large scale.

## Credentials

- Never read or set `ANTHROPIC_API_KEY` anywhere in this repo (code, `.env`, scripts, tests).
  The app's agents use `KODEHUB_ANTHROPIC_API_KEY`. If `ANTHROPIC_API_KEY` is present, Claude Code
  bills API credits instead of the developer's subscription.
- The default agent backend drives the official `claude` CLI and uses the developer's own Claude
  login, so a Pro/Max subscription works without any API key. Kodehub must never read, store or
  forward Claude credentials, and must never send a subscription OAuth token to the Messages API.
- Milestones M0–M4 need no API key. M5 tests must pass in recorded-response mode without a key.
