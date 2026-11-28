# Kodehub ontology

The graph model Kodehub stores in ArangoDB: every node collection, every edge collection, and the
rules that tie them together. It replaces the collection lists in `docs/02-data-model.md` and
the release model in `docs/06-releases-environments.md` (decisions D-055 to D-062).

Source drawing: the "Kodehub ontology" Google Drawing (25 Nov 2026 revision). This document is
the authoritative text version; where it adds an edge the drawing leaves unnamed, the name here
wins. Where the implementation deviates from the drawing, the deviation is listed in
[Deviations from the drawing](#deviations-from-the-drawing) with its reason.

## Conventions

- **Collections** are snake_case and plural (`code_versions`, `deploy_logs`). The one
  exception is `code`, which is already a mass noun.
- **Edges** are snake_case verb phrases read from `_from` to `_to`: `requirement applies_to
  component`, `deployment uses deployer`. One edge collection may connect several pairs of
  node collections when the meaning is the same (`has_version`, `contains`, `logs_to`).
- **Documents** carry `created_at` (RFC 3339) and, where a person or agent made them,
  `created_by`.

## Tenancy: one set of collections for every project

Every project shares the same collections (no per-project collections, decision D-055).

- Every project-scoped document and edge has a `project` attribute (the project id), backed by
  a persistent index.
- Every project-scoped `_key` starts with the project id and a dot: `<project>.<local>`, for
  example `code/shop.rust-hash_password` or `code_versions/shop.9f2c…`. Ids are readable and
  unique across projects, and "belongs to exactly one project" (hard rule 9) is checkable from
  the key alone.
- The store refuses any insert whose `_key`, `_from` or `_to` belongs to a different project
  than the request's, and every collection scan is filtered to the request's project. Edges
  never cross projects (except to the global `projects` node of the same project).
- A future disjoint SmartGraph sharded on `project` needs no data change.

## Hubs

The drawing has hub boxes between the cluster and high-volume collections (`orgs`, `users`,
`remote_agents`, `work_queue`, `audit_logs`). They exist so that traversing from a project or
the cluster never drags in tens of thousands of work items or audit entries. In ArangoDB the
collection itself is the hub: "all work items" is an indexed query on `work_items`, and no edge
is written from a project or the cluster to each item (decision D-056). `kh_cluster` is the
singleton `meta/cluster` document.

## Node collections

### Global (cluster-wide)

| Collection | One document is | Notes |
|---|---|---|
| `projects` | a project ("My Project") | Key is the project id. `owner` is the org or user whose credentials and settings layer apply; access is separate (`has_access`). `current_version` names the default open `project_version`. |
| `orgs` | an organization ("Arango") | |
| `users` | a human user ("bobby") | |
| `remote_agents` | an external AI client that signs in like a user | Can add work items, edit requirements, start tests. Never changes code, builds or deploys. |
| `secrets` | a credential (an LLM API key, a cloud key) | Holds ciphertext only. What it unlocks is a `secures` edge. Project-scoped secrets (deployer keys) carry `project`. |
| `work_items` | one unit of queued work | Global so every orchestrator replica can claim across projects; carries `project`. Holds the work's proposal (plan, candidate versions, change log, review findings, test results) — see [Work without changesets](#work-without-changesets). |
| `audit_log` | one audited action | Append-only. `actor`, `on_behalf_of`, `project`, `action`, `target`, `at`. |

### Project-scoped

| Collection | One document is | Mutable? |
|---|---|---|
| `philosophy` | the project's philosophy document (one per project) | yes (text history kept on the document) |
| `requirements` | a requirement ("always do X") | yes (presentation only; content lives in versions) |
| `requirement_versions` | one revision of a requirement | no |
| `use_cases` | a user-testable scenario ("user logs in") | yes |
| `agents` | an internal agent type for this project ("coding agent"): its start-up prompt and the actions it may take | yes |
| `agent_chats` | a conversation thread between agents, or between a human and agents | yes |
| `agent_chat_msgs` | one message in a thread | no |
| `container_images` | an image used to test, build or deploy ("test_rust", "deploy_k8s") | yes |
| `schedules` | a recurring job (a test cadence, a backup, a Git export) | yes |
| `contexts`, `containers`, `components` | the unversioned C4 nodes | yes (presentation and lifecycle only) |
| `code` | an unversioned code node: a function, class, trait or enum | yes (presentation only) |
| `tests` | an unversioned test | yes (presentation only) |
| `project_versions` | the version root of the whole C4 tree; a release | while **open** its tree moves; once **frozen**, never |
| `context_versions`, `container_versions`, `component_versions` | an immutable C4 node version: its definition plus the exact child versions it holds | no |
| `code_versions` | an immutable code version (signature, body, attributes) | no |
| `test_versions` | an immutable test version | no |
| `builds` | a build artifact, keyed by `closure_hash`; the bytes live in object storage | no |
| `test_logs` | one test execution (or one ad-hoc run) and its result | no |
| `environments` | a stage of the promotion ladder ("dev", "test", "qa") | yes |
| `deploy_targets` | a logical place code runs ("aws"); the environment decides the real infrastructure | yes |
| `deployers` | the setup for one deploy target in one environment ("aws in dev"): endpoint, image, secret | yes |
| `deployments` | one deployment of a project version into an environment; retryable | yes (state, attempts) |
| `deploy_logs` | one step or attempt of a deployment | no |

## Edge collections

Cardinality is written from → to. "M:M" means many on both sides.

### Identity and access (global)

| Edge | From → To | Card. | Meaning |
|---|---|---|---|
| `works_with` | `users` → `orgs` | M:M | the user is a member of the org; `role` (member, admin) |
| `acts_for` | `remote_agents` → `users` \| `orgs` | M:M | the remote agent works on behalf of the user (or of an org, for agents an org admin creates — spec §13A.1) |
| `has_access` | `users` \| `orgs` → `projects` | M:M | `permission`: read, write or admin |
| `secures` | `secrets` → `orgs` \| `users` \| `projects` \| `remote_agents` \| `deployers` | 1:M | the secret is the credential of that holder (model and Git credentials are layered: user, org, project) |

### Project structure

| Edge | From → To | Card. | Meaning |
|---|---|---|---|
| `owns` | `projects` → `philosophy` \| `requirements` \| `use_cases` \| `agents` \| `agent_chats` \| `container_images` \| `schedules` \| `environments` \| `deploy_targets` | 1:M | top-level membership, for traversal from the project |
| `contains` | `projects` → `contexts` → `containers` → `components` → `code`; `contexts` \| `containers` \| `components` → `tests` | 1:M | the unversioned C4 hierarchy |
| `contains` | `project_versions` → `context_versions` → `container_versions` → `component_versions` → `code_versions`; `context_versions` \| `container_versions` \| `component_versions` → `test_versions` | 1:M | the version tree (see below) |
| `has_version` | `projects` → `project_versions`; `contexts` → `context_versions`; `containers` → `container_versions`; `components` → `component_versions`; `code` → `code_versions`; `tests` → `test_versions`; `requirements` → `requirement_versions` | 1:M | a node's versions. `state`: candidate, accepted or discarded. `work_item` that made it. |
| `based_on` | `project_versions` → `project_versions` | M:1 | a release started from another (the next release, or a hotfix of a frozen one) |

### Code relationships (between unversioned nodes)

| Edge | From → To | Card. | Meaning |
|---|---|---|---|
| `calls` | `code` → `code` | M:M | optional `pinned_version` |
| `depends_on` | `code` → `code` | M:M | types, traits and other non-call dependencies; optional `pinned_version` |
| `member_of` | `code` → `code` | M:1 | a method belongs to its class |
| `binds_to` | `code` → `code` | M:1 | a Python wrapper binds a Rust symbol (PyO3) |
| `tested_with` | `code` \| `components` \| `containers` \| `contexts` → `tests` | M:M | the node is exercised by the test |

Which *version* of the target these edges mean is resolved through the project version being
built or read: the version of the target reachable from that project version's root.

### Intent: requirements, use cases, tests

| Edge | From → To | Card. | Meaning |
|---|---|---|---|
| `applies_to` | `requirements` → `projects` \| `contexts` \| `containers` \| `components` \| `code` | M:M | the requirement is attached at that level and inherited by everything below |
| `verified_by` | `requirements` \| `use_cases` → `tests` | M:M | the test proves the requirement or the use case |
| `relates_to` | `use_cases` → `requirements` | M:M | the requirements a use case exercises |

### Agents and conversation

| Edge | From → To | Card. | Meaning |
|---|---|---|---|
| `has_message` | `agent_chats` → `agent_chat_msgs` | 1:M | thread membership |
| `authored` | `agents` \| `users` \| `remote_agents` → `agent_chat_msgs` | 1:M | who wrote the message |
| `refers_to` | `agent_chat_msgs` → any project node or `work_items` | M:M | a context pointer the author chose |

### Work and schedules

| Edge | From → To | Card. | Meaning |
|---|---|---|---|
| `has_work_item` | `project_versions` → `work_items` | 1:M | the open release the work's accepted changes go into |
| `intends_changes` | `work_items` → `contexts` \| `containers` \| `components` \| `code` \| `tests` | M:M | the declared intent: what the work will change (overlap and negotiation are computed from these) |
| `targets` | `schedules` → `projects` \| `contexts` \| `containers` \| `components` \| `code` \| `tests` | M:1 | what the schedule acts on |

### Build, test, deploy

| Edge | From → To | Card. | Meaning |
|---|---|---|---|
| `build_of` | `builds` → `code_versions` \| `component_versions` \| `container_versions` \| `test_versions` | M:M | the versions a build was made from |
| `built_for` | `builds` → `deploy_targets` | M:1 | the target the build is for |
| `logs_to` | `test_versions` \| `builds` → `test_logs`; `deployments` → `deploy_logs` | 1:M | where results are written |
| `deployed_to` | `context_versions` \| `container_versions` \| `component_versions` \| `code_versions` → `deploy_targets` | M:1 | cloud-specific versions name their target; versions without one use the project's default target |
| `has_deployment` | `project_versions` \| `environments` → `deployments` | 1:M | which release, and which environment |
| `uses` | `deployments` → `deployers` | M:M | the deployers that carried the deployment out |
| `has_deployer` | `environments` \| `deploy_targets` → `deployers` | 1:M | a deployer is exactly one (environment, deploy target) pair |
| `runs` | `deployers` → `container_images` | M:1 | the image that performs the deploy |

## The version tree

Unversioned nodes (`contexts`, `containers`, `components`, `code`, `tests`) are stable anchors:
requirements, intents, work items, schedules and test history attach to them and survive every
change. Their content lives in immutable version nodes.

- A **project version** is the root. While **open** it is the release being worked on: its
  `contains` edges point at the current context versions and move as work is accepted. When it
  is **frozen** (by the orchestrator or an architect agent, e.g. a milestone is reached, or on
  its first deploy beyond DEV), it never changes again.
- Every C4 version lists **all** of its children, changed or not, through `contains` edges
  written once when the version is created.
- **Version keys are content-addressed.** A code or test version's `content_hash` is the hash
  of its canonical content (hard rule 3). A context, container or component version's
  `content_hash` is the hash of its definition, and its `tree_hash` also covers the sorted ids
  of its children. A version's key is derived from its node and its tree hash, so identical
  subtrees on two releases are the same documents.
- **Changing code** writes the new code version, then one new version per ancestor level (each
  pointing at the new child and the unchanged siblings), then moves the open project version's
  `contains` edge — one ArangoDB transaction, with a compare-and-set on the project version.
- **Reading a release** is one walk down from its project version. Cost does not depend on how
  many releases exist.
- **"Which version of node N"** always means "in project version P": the version of N reachable
  from P. With no P given, the project's `current_version` is used.
- A **hotfix** is a new open project version `based_on` a frozen one, starting with the same
  tree.
- Requirements are versioned the same way but outside the tree: `requirements → has_version →
  requirement_versions`; the requirement's current revision is the newest accepted version.

### Version states

| State | Meaning |
|---|---|
| candidate | written by an agent for a work item, not yet in any project version |
| live | reachable from the project's current (default) open project version |
| retired | accepted once, no longer reachable from any open project version |
| discarded | a candidate that was rejected or withdrawn |

`has_version.state` records candidate / accepted / discarded; live and retired are computed
from the tree.

## Work without changesets

There is no changeset node (decision D-058). Code changes are made only by internal agents, one
work item at a time:

1. A work item is queued against an open project version (`has_work_item`) and declares what
   it will change (`intends_changes`). Overlap with other work items' intents decides whether
   it proceeds, waits, or is absorbed (spec §10); negotiations happen in an `agent_chats`
   thread that refers to both work items.
2. The agent writes **candidate** code and test versions (`has_version.state = candidate`,
   `work_item` set). Nobody else sees them in any tree.
3. Builds and formal tests run against the project version with the candidates overlaid; a
   blind review records its findings on the work item.
4. If the gates pass, the orchestrator **accepts** the candidates: it builds the ancestor
   chain against the project version's tree *at that moment* and moves its root, in one
   transaction. Unrelated work accepted meanwhile is never lost, and a change whose
   dependencies moved underneath it is retested (its closure hash changed).
5. The work item keeps the evidence: plan, change log, review findings, test results. Each new
   version records its `work_item`.

## Deploying

```
project_version ─has_deployment→ deployment ←has_deployment─ environment
                                    │  ╰─uses→ deployer ←has_deployer─ environment
                                    │             ╰←has_deployer─ deploy_target
                                    ╰─logs_to→ deploy_log
```

- A deployment is one project version going into one environment. It is retryable: its state
  is pending → (awaiting approval) → running → succeeded | failed, and every attempt and step
  is an immutable `deploy_logs` entry.
- The deployer for each needed deploy target *in that environment* performs it. In DEV the
  `aws` target's deployer may be a local Kind namespace; in TEST it may be real AWS. The code
  never changes between environments.
- All deployers a deployment uses belong to its environment (checked on write).
- Promotion gates stay per environment (DEV → TEST: everything builds; TEST → QA: formal tests
  pass, no orphans, live use cases pass; QA → PRE-PROD: smoke run; PRE-PROD → PROD: human
  approval, recorded on the deployment as `approved_by` / `approved_at`).
- A project version is frozen on its first deployment beyond the lowest environment.

## System collections (not part of the graph)

Plumbing that holds no domain knowledge: `meta` (cluster singleton, schema version),
`settings` (layered settings), `sessions`, `tokens` (personal and internal access tokens,
hashed), `oauth_clients`, `oauth_codes`, `usage` (token metering), `notifications`,
`schedule_triggers` (event-driven schedule firings), and `import_staging` (a Git import's parsed
units while the import agent maps them; project-scoped like graph data, but no edges touch it).

## Mapping from the v0.1 build

| v0.1 | Now |
|---|---|
| per-project collections `<project>__x` | shared `x` with a `project` attribute and `<project>.` keys |
| `kodehub_users` (kind human) / (kind agent) | `users` / `remote_agents` |
| `kodehub_member_of`, `kodehub_access`, `kodehub_acts_for` | `works_with`, `has_access`, `acts_for` |
| `kodehub_credentials` | `secrets` + `secures` |
| `kodehub_work_items`, `kodehub_audit`, `kodehub_projects` | `work_items`, `audit_log`, `projects` |
| `symbols` (kind test) | `tests` |
| `symbols` (other kinds) | `code` |
| `versions` | `code_versions` / `test_versions` |
| `c4_versions` | `context_versions` / `container_versions` / `component_versions` |
| stable `has_version` edge with `validFrom` / `validTo` | reachability from the current project version |
| `changesets`, `proposes` | the work item's proposal + candidate versions |
| `releases`, `supports`, `cloned_from` | `project_versions`, the tree, `based_on` |
| `in_environment` | `deployments` |
| `artifacts`, `built_from` | `builds`, `build_of` |
| `test_runs`, `runs` | `test_logs` (`kind`: test or run) |
| `test_container_types` | `container_images` |
| `agent_types` | `agents` |
| `chat_messages`, `negotiations` | `agent_chats` + `agent_chat_msgs` |
| `intents` | the work item's `intent` + `intends_changes` edges |
| `requirement_history` | `requirement_versions` |
| `requirement_groups`, `includes` | requirement `tags` (display grouping) |
| `satisfies` (node → requirement) | `applies_to` (requirement → node) |
| `verifies` (test → requirement), `expected_by` | `verified_by` (requirement \| use case → test) |
| `tested_by` | `tested_with` |
| `missions`, `traces_to` | the philosophy document |
| `decisions`, `justified_by` | requirements of `kind: decision` (`rationale`, `alternatives`) |
| `approvals` | the state of the thing awaiting approval (proposed requirement version, proposed context, a plan awaiting approval on its work item, a deployment awaiting approval) |
| `namespaces`, `exposes` | `public_name` / `namespace` on the code node (render projection) |
| `build_targets`, `compiles_to` | `deploy_targets` + `built_for` |
| `import_units` | `import_staging`, a system collection (the import staging set is not graph data) |

## Deviations from the drawing

1. **`tested_with` and the dependency loop connect unversioned nodes, not versions.** The
   drawing puts `tested_with` and "C4-to-C4 object recursive dependencies" on the version side.
   Stored there, every new version of a callee would force new versions of all its callers and
   tests (immutable versions cannot gain edges), cascading through the graph. Kept on the
   unversioned nodes, the version meant is resolved through the project version, which is
   exactly what `closure_hash` captures.
2. **Test versions sit in the version tree.** The drawing shows `test → has_version →
   test_version` but no tree edge into test versions. Without one, a release would not know
   which test versions it contains. Component (or container, or context) versions `contains`
   their test versions.
3. **Hubs are collections, not documents** (see [Hubs](#hubs)).
4. **`has_work_item` points from the project version to the work item** as drawn; work items
   stay global records so orchestrator replicas can claim across projects.
