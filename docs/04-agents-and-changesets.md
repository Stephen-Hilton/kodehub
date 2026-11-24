# 04 — Agents and changesets

> **v0.05 design document.** Superseded wherever it conflicts with
> `docs/Kodehub v0.1 Requirements Layered Spec for Claude Code.md` (see D-041).

## Principle

Humans state intent and refine requirements. Agents do everything else. Every agent action goes
through the same graph API the UI uses, and every change is traceable to a requirement.

## Agent roles

| Agent | Responsibility | Context it may see |
| --- | --- | --- |
| **Orchestrator** | Talks with the human. Turns intent into requirements and plans. Opens changesets, assigns work, resolves disputes, escalates. | Everything. |
| **Coder** | Writes and changes code versions within a changeset. | Requirements, mission, decisions, relevant code. |
| **Reviewer** | Blind critical review. | Requirements, mission, the changeset's diff and tests. **Not** the coder's reasoning or conversation. |
| **Tester** | Writes tests from requirements (examples, edge cases, constraints), not from code. Runs tests. | Requirements, signatures. Code bodies only when debugging a failure. |
| **Environment agents** (dev, test, qa, prod) | Deploy releases to their environment and gate promotion. | Releases, test results, environment config. |

Agent prompts live in `crates/kodehub-agents/prompts/*.md` so they can be tuned without code changes.

## Agent runtime (pluggable)

Agents are defined once (prompt + allowed tools) and run on a pluggable backend behind an
`AgentBackend` trait in `kodehub-agents`. Their tools are the Kodehub MCP server's tools (thin
wrappers over the graph API). Agents have no file-system or shell access.

| Backend | How it runs | Auth / billing | Use |
| --- | --- | --- | --- |
| **Claude Code** (default) | Spawns the official `claude` CLI in headless mode (`-p`) with streaming JSON output, the Kodehub MCP server configured, built-in file/shell tools disabled and only `mcp__kodehub__*` tools allowed, and the agent's prompt appended to the system prompt. One fresh session per agent task. | The developer's own Claude login (`claude login`), or `CLAUDE_CODE_OAUTH_TOKEN` from `claude setup-token` for headless machines. Draws from the developer's Claude plan. | Individual developers on Pro/Max. |
| **API** | Direct Messages API calls with tool use. | `KODEHUB_ANTHROPIC_API_KEY` (a Console key; Max/Team monthly API credits apply). | Teams server, CI, or anyone without Claude Code. |
| **Recorded** | Replays stored responses. | None. | Deterministic tests and CI. |

Rules:
- Kodehub never reads, stores or forwards the developer's Claude credentials. The Claude Code
  backend relies on the CLI's own authentication.
- Never pass a subscription OAuth token (`sk-ant-oat01-…`) to the Messages API.
- Verify exact CLI flags against the current Claude Code documentation when implementing; record
  them in `docs/decisions.md`.
- Blind review isolation holds on every backend: the reviewer always runs in a fresh session whose
  input is built only from requirements, mission, diff and tests.
- Settings: backend choice and model per agent, in `~/.kode/config.toml`.

## Changeset lifecycle

```
draft → planned → approved → building → reviewing → testing → ready → merged
                                  ↑          │            │
                                  └──────────┴────────────┘ (rework, max 3 rounds)
any state → rejected | escalated
```

1. **draft** — orchestrator creates it from a human request or a requirement edit.
2. **planned** — orchestrator attaches the plan: symbols to add or change, requirements affected.
   The UI shows proposed nodes as **ghost nodes** (dotted outlines).
3. **approved** — the human approves or adjusts the plan. (Configurable: auto-approve for small
   changes in a later phase.)
4. **building** — coder proposes new versions (`proposes` edges). Nothing is current yet.
5. **reviewing** — reviewer works blind and returns findings: `{ severity, location, finding }`.
6. **testing** — tester writes or updates tests from the requirements; all affected tests run
   against the changeset's proposed closure.
7. Coder and reviewer may disagree. They exchange up to 3 rounds; the outcome is recorded as a
   **Decision** node. No consensus → **escalated** to the human.
8. **ready** — merge checks pass:
   - no orphans introduced,
   - every affected requirement's tests pass,
   - reviewer has no unresolved blocking findings.
9. **merged** — all `current_version` edges move in one transaction; artifacts are already cached.

## Requirement edits drive work

When a human edits a requirement (e.g. adds an edge case), every node that satisfies it or is
reachable below it is marked **at risk** (amber) and the orchestrator automatically opens a
changeset to address it.

## Graph API (HTTP and MCP)

Core operations, exposed identically over HTTP and as MCP tools:

- `get_node`, `query_nodes`, `traverse` (with direction, edge types, depth)
- `create_requirement`, `update_requirement`, `create_decision`
- `open_changeset`, `propose_version`, `propose_edge`, `submit_review`, `merge_changeset`,
  `reject_changeset`
- `coverage(requirement_id?)`, `orphans()`, `impact(symbol_id)`, `duplicates()`
- `run_tests(scope)`, `run_now(symbol_id, args, environment)`
- `cut_release`, `promote(release_id, environment)`, `rollback(environment)`
