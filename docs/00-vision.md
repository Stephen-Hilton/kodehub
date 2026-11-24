# 00 — Vision

> **v0.05 design document.** Superseded wherever it conflicts with
> `docs/Kodehub v0.1 Requirements Layered Spec for Claude Code.md` (see D-041).

## Core principle

**Humans face the future. AI handles the past and present.**

Humans bring information that doesn't exist yet: strategy, product direction, new ideas,
judgement calls, policy. AI works from everything already known and does the building, testing,
validating and operating.

## The problem

Today's tooling (text files, Git, pull requests, file-based CI, team boundaries) was built around
human limits: slow typing, small batches, line-by-line review, and context that takes people
decades to build and can't be copied. Agents don't share those limits. Context can be copied
instantly, so many boundaries that existed to split work between people become unnecessary.

## What changes

- The valuable asset moves from the codebase to the **context**: requirements, decisions,
  rationale, incident lessons. Code becomes a regenerable by-product.
- Code lives in a **graph**, connected by meaningful edges to the requirements it satisfies, the
  tests that verify it and the decisions that justify it. Files are a temporary view.
- Humans work by **refining requirements** (constraints, examples, edge cases), not editing code.
- **Validation becomes a query, not a review**: does every requirement have passing tests? Does
  any code exist that no requirement reaches?
- Discrete, shared function nodes mean **less code and more reuse**. A bug fixed once is fixed for
  every consumer.
- The interface is **mission control**, not an editor: a navigable map of the system plus a
  conversation with an orchestrator agent.

## What the pilot must demonstrate

1. Code can live in a graph and still compile, test and run as Rust and Python.
2. A human can change a requirement and agents carry the change all the way through code, tests
   and build, with full traceability.
3. Coverage, orphan and impact questions are answered by graph queries.
4. "Run now" on a code node works instantly when nothing has changed.
5. Releases are cheap, immutable and promotable across environments.
