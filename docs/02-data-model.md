# 02 — Data model (ontology)

> **v0.05 design document.** Superseded wherever it conflicts with
> `docs/Kodehub v0.1 Requirements Layered Spec for Claude Code.md` (see D-041).

Two layers:

- **Core layer** — AI-native. Everything identified by hash or stable ID; names are labels.
- **Projection layer** — exists for humans and toolchains (namespaces, display names, files).
  Expected to shrink over time.

Code and context live in separate collections, joined by edge collections into one named graph
`kodehub`.

## Identity: symbols and versions

Code is split into a stable **symbol** (the logical thing, e.g. "the hash_password function") and
immutable **versions** of it.

- `content_hash` — BLAKE3 of the version's canonical content only. Used for identity and
  duplicate detection. Two versions with identical content have the same hash and are the same node.
- `closure_hash` — BLAKE3 of `content_hash` plus the sorted `closure_hash`es of everything it
  depends on (resolved to current versions, or pinned versions where pinned). Used as the build
  and test cache key. A change in any dependency changes the closure hash of every dependent.

**Canonical content** for hashing: JSON with sorted keys, no insignificant whitespace, containing
`kind`, `language`, `signature`, `body` (line endings normalised to `\n`, trailing whitespace
stripped) and `attributes`. Display names are **not** hashed.

## Document collections

### Context collections

| Collection | Purpose | Key fields |
| --- | --- | --- |
| `missions` | Overarching goal; pinned in UI | `title`, `statement`, `scope` (`program` or a context id) |
| `requirements` | One discrete requirement | see anatomy below |
| `requirement_groups` | Bundle of requirements/groups | `title`, `logic` (`all_of` \| `any_of`) |
| `decisions` | A recorded choice and why | `title`, `choice`, `rationale`, `alternatives`, `made_by` (`human` \| agent id), `created_at` |

**Requirement anatomy** (fields on `requirements`):

- `statement` — the core requirement.
- `intent` — why it exists.
- `constraints[]` — rules and limits.
- `examples[]` — `{ description, input, expected }`.
- `edge_cases[]` — `{ description, input, expected }`, added over time.
- `out_of_scope[]` — what it deliberately doesn't cover.
- `revision` — integer, incremented on every edit (keep prior revisions in `requirement_history`).

Humans refine requirements instead of creating new ones; a bug fix is usually a new edge case.

### Architecture collections (C4)

| Collection | Example |
| --- | --- |
| `contexts` | Authentication, Data |
| `containers` | auth-service, ArangoDB, Redis |
| `components` | credential-verification, password-hashing |

### Code collections

| Collection | Purpose | Key fields |
| --- | --- | --- |
| `symbols` | Stable identity of a code item | `kind` (`function` \| `class` \| `trait` \| `enum` \| `test`), `language` (`rust` \| `python`), `display_name`, `visibility` (`public` \| `internal`), `tags[]` |
| `versions` | Immutable content | `_key` = `content_hash`, `kind`, `language`, `signature`, `body`, `attributes`, `created_at`, `created_by` |
| `build_targets` | Where code compiles to | `language`, `os` (`linux` \| `macos` \| `windows`), `kind` (`rust-lib`, `rust-bin`, `python-pkg`, `python-ext`) |
| `artifacts` | Cached builds | `closure_hash`, `build_target`, `path`, `built_at`, `status` |

A method is a `function` symbol with a `member_of` edge to a `class` symbol. Tests are symbols of
kind `test` with versions, like any other code.

### Projection collection

| Collection | Purpose |
| --- | --- |
| `namespaces` | Human-readable publishing surface (e.g. a community SDK). Maps to a Rust `mod` or a Python package path at render time. |

### Workflow collections

| Collection | Purpose |
| --- | --- |
| `changesets` | Proposed set of changes (graph equivalent of a PR). See doc 04. |
| `test_runs` | `{ test_version, closure_hash, status, output, duration_ms, ran_at }` |
| `runs` | Run-now executions: `{ version, closure_hash, environment, args, result, stdout, stderr, ran_at }` |
| `releases` | Immutable manifest. See doc 06. |
| `environments` | `dev`, `test`, `qa`, `prod` |

## Edge collections

| Edge | From → To | Notes |
| --- | --- | --- |
| `contains` | mission→context, context→container, container→component, component→symbol | C4 hierarchy |
| `member_of` | symbol(function) → symbol(class) | Methods |
| `current_version` | version → symbol | **Exactly one per symbol** |
| `previous_version` | version → symbol | `{ superseded_at, superseded_by }` |
| `calls` | symbol → symbol | Optional `{ pinned_version }` |
| `depends_on` | symbol → symbol | Types, traits, non-call dependencies; optional `pinned_version` |
| `tested_by` | symbol → symbol(test) | |
| `verifies` | symbol(test) → requirement | |
| `satisfies` | any C4 node or symbol → requirement \| requirement_group | Requirements may attach at any level |
| `justified_by` | any → decision | |
| `includes` | requirement_group → requirement \| requirement_group | |
| `traces_to` | requirement \| requirement_group → mission | |
| `binds_to` | symbol(python) → symbol(rust) | PyO3 wrapper relationship |
| `compiles_to` | symbol \| component → build_target | |
| `exposes` | namespace → symbol | `{ public_name }` |
| `proposes` | changeset → version \| requirement | |
| `included_in` | version → release | |
| `deployed_to` | release → environment | `{ deployed_at, deployed_by }` |
| `currently_running` | environment → release | **At most one per environment** |
| `previously_ran` | environment → release | `{ from, until }` |

## Versioning rule

Writing a new version of symbol S:

1. Canonicalise and hash. If a version with that hash exists, reuse it.
2. Move S's `current_version` edge to `previous_version` (with timestamps).
3. Create a `current_version` edge from the new version to S.

Steps 2–3 happen in one ArangoDB transaction.

## Traceability semantics

- **Satisfaction propagates downward.** A requirement attached to node N is satisfied only if every
  test that verifies it passes **and** every test attached to anything reachable from N (via
  `contains`, `calls`, `depends_on`, `member_of`) passes, for current closure hashes.
- **Helpers are covered transitively.** A function needs no requirement edge of its own if a
  requirement-tagged node reaches it.
- **Orphan:** a symbol not reachable from any node that has a `satisfies` edge.
- **Impact:** from a symbol, traverse inbound `calls`/`depends_on`/`contains` edges upward to find
  every requirement at risk.

These three queries (coverage, orphans, impact) are implemented in AQL in `kodehub-store` and
exposed through the API.

## Duplicate detection

- Exact duplicates: two symbols whose current versions share a `content_hash`. Report as a merge
  candidate.
- Near duplicates (milestone 5+): embed version bodies and flag pairs above a similarity
  threshold for the orchestrator to consider merging.
- Merge guardrail (to tune): split a merged function again when its option branches share less
  than about half of their logic, or when tests for different options conflict.
