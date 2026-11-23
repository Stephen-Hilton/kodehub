# 05 — Desktop interface

No file tree, no text editor at the centre. The interface is a **map** of the system plus a
**conversation** with the orchestrator.

## Layout

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Mission banner (pinned)                                                  │
├───────────────┬─────────────────────────────────────────┬───────────────┤
│ Requirements  │                                         │ Agent team    │
│ & test status │            System map                   │ & activity    │
│               │      (semantic zoom, health colours)    │               │
│               │                                         │ Orchestrator  │
│               │                                         │ chat          │
├───────────────┴─────────────────────────────────────────┴───────────────┤
│ Time slider (versions / releases)        │  Environment: dev ▾           │
└─────────────────────────────────────────────────────────────────────────┘
```

## Mission banner

Always visible. Shows the program mission; when zoomed into a context with its own mission,
shows that one too.

## System map

- Cytoscape.js with compound nodes. Levels: Context → Container → Component → Symbol.
- **Semantic zoom:** zoomed out shows contexts; zooming in or double-clicking expands children.
- **Health colours**, computed from coverage at the current closure hashes:
  green = all tests beneath pass; amber = at risk (requirement changed, untested change, or stale
  results); red = a test beneath is failing; grey = no requirement reaches it (orphan).
- **Ghost nodes** (dotted) for changes proposed in an open changeset.
- Selecting a requirement in the left sidebar highlights its full path: satisfying nodes, tests,
  decisions.

## Left sidebar — requirements

- Tree of requirement groups and requirements with pass/fail/at-risk badges.
- Opening a requirement shows its anatomy (statement, intent, constraints, examples, edge cases,
  out of scope) as an editable form. Editing it is the main way humans change the system.
- "Add edge case" is a first-class, one-click action.

## Right sidebar — agents

- Agent list with status, current task and to-do list.
- Open changesets with their state, reviewer findings and confidence flags.
- Orchestrator chat: the primary way to ask for new work. Messages can reference nodes by clicking
  them on the map.

## Node panel (opens on selecting a symbol)

- Signature, health, requirements it serves (upward), dependencies (downward), version history.
- **Run now** with generated parameter form, environment selector and output stream;
  **Save as example** after a run.
- **Text view** of the current version body (secondary). Editable for manual fixes; saving
  creates a changeset that still goes through review and tests.

## Embedded terminal

A dockable terminal panel (like VS Code's) where the developer can run interactive Claude Code or
any shell command. When Claude Code runs here, Kodehub offers to register its MCP server so that
Claude Code can read and change the graph through the same API as the built-in agents.

## Time slider

Scrub through merged changesets and releases; the map shows the system as it was. Compare two
points to see which symbols changed.

## Human workflow summary

1. **Create** — ask the orchestrator or write a requirement; approve the ghost-node plan.
2. **Adjust** — refine a requirement (usually an edge case) or talk about a node in place.
3. **Review** — inspect changesets on the map, with tests and reasoning; act on flagged items.
4. **Inspect** — zoom, scrub history, run nodes, open text view if truly needed.
