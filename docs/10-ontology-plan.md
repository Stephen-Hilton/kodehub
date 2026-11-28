# 10 — Moving the build to the ontology

Plan for bringing the v0.1 codebase onto `docs/ontology.md`. Decisions D-055 to D-062 in
`docs/decisions.md` record the choices; this file is the step plan with acceptance criteria.
Steps run in order; each ends with `make fmt lint test` green before the next starts. The
database is wiped (no migration of existing data).

## Starting point (audit, 25 Nov 2026)

- 98 Rust tests pass (`make fmt lint test-unit test-integration`), 35k lines of Rust, 5k of
  TypeScript.
- Tenancy: per-project collections `<project>__<name>` behind a transparent mapping layer in
  `kodehub-store/src/arango.rs`; one named graph per project with every edge allowed between
  every collection.
- Versioning: one global "stable" `has_version` edge per node, `validFrom` / `validTo`
  intervals, a unique sparse index on `stable_of`. Code and tests share `symbols` / `versions`;
  C4 levels share `c4_versions`. There are no edges between versions.
- Changes: `changesets` documents (proposals, reviews, test results, a 10-state machine) driven
  by `kodehub-agents/src/engine.rs`; candidates are `has_version` edges tagged with the
  changeset.
- Releases: `releases` with `supports` edges to every selected version (selection rules),
  `in_environment` edges with validity intervals, `cloned_from` for patch releases.
- Requirements: `requirements` + `requirement_history`, groups (`requirement_groups`,
  `includes`), attached by `satisfies` (node → requirement); tests attach by `verifies`.

## Steps

### Step 1 — Shared collections (D-055)

- `arango.rs`: logical name = physical name; no per-project collections or graphs. Inserts into
  project collections get `project`, and their `_key` / `_from` / `_to` must carry the project's
  `<project>.` prefix. `FOR x IN @@coll` scans over project collections are filtered to the
  request's project automatically. Reads by key refuse another project's key.
- Store helper `Store::pkey(local)` builds qualified keys; every key construction goes through
  it.
- Schema: one bootstrap of every collection, persistent indexes on `project`, unique indexes
  rewritten as `(project, …)`, one named graph `kodehub` whose edge definitions list the exact
  from/to collections of the ontology.
- Project delete removes the project's documents instead of dropping collections.

Acceptance: two projects with the same content (same code, same environments) coexist; a scan
in one never returns the other's documents (test); inserting a foreign key is refused (test);
the number of collections no longer grows with projects (test).

### Step 2 — Global renames and identity (D-056, D-061)

- `kodehub_*` globals → `projects`, `users`, `orgs`, `remote_agents`, `work_items`,
  `audit_log`, `secrets`, and the system collections listed in the ontology.
- Agent users (`kind: agent`) move to `remote_agents`; `acts_for` is `remote_agents → users`.
- `works_with` (user → org), `has_access` (user | org → project), `secures` (secret → holder).
- Remote agents may create work items, edit requirements and start tests; the API refuses them
  any code-changing, build or deploy call (test).

Acceptance: existing identity, OAuth and token tests pass on the new names; remote-agent
restrictions tested.

### Step 3 — The version tree (D-057)

- New collections `code`, `tests`, `code_versions`, `test_versions`, `context_versions`,
  `container_versions`, `component_versions`, `project_versions`.
- Version keys: `<project>.<hash(node, tree_hash)>`; `content_hash` unchanged for code (hard
  rule 3).
- `kodehub-store/src/tree.rs`: resolve a project version's tree (node → version, parents),
  cached per project version revision; `apply_tree_changes(project_version, changes)` builds
  new ancestor versions bottom-up and moves the root in one transaction with a compare-and-set
  on the project version.
- Every "stable version of N" read becomes "version of N in project version P" (default: the
  project's `current_version`). `validFrom` / `validTo`, `stable_of` and the overlap invariant
  checks are removed.
- Closure hashes are computed per project version (not stored on code nodes).
- Creating a project creates its first open project version (`r1`) and makes it current.

Acceptance (from the versioned-releases prompt): reading a project version costs one walk;
two project versions share unchanged version documents (same ids); changing one code node
writes exactly one new version per ancestor level plus their child edges; requirements and
intents still attach to unversioned nodes; history of a node lists its versions with the
project versions that contain them. All tests.

### Step 4 — Work items replace changesets (D-058)

- The changeset document becomes the work item's `proposal` (same fields: plan, proposals,
  edges, reviews, test results, activity) and its stage (planned → approved → building →
  reviewing → testing → ready → accepted | rejected | escalated).
- `propose_version` writes candidate code/test versions (`has_version.state = candidate`,
  `work_item`); `accept` replaces `merge`: gates as before (orphans, affected formal tests,
  blind review), then `apply_tree_changes` on the work item's project version.
- Intents: `intends_changes` edges + `intent` on the work item; negotiations: an
  `agent_chats` thread (`kind: negotiation`) referring to both work items, one message per
  round.
- Engine, orchestrator, MCP tools and HTTP routes move from `/api/changesets/…` to
  `/api/work-items/{key}/…`.

Acceptance: the agent demo (plan → tests first → code → blind review → formal tests →
accept) runs on recorded responses; all-or-nothing acceptance; orphan and failing-test gates
refuse; review gate refuses when `blind_review` is on and passes when off; two work items on
disjoint components accepted in either order both land (no lost update); absorbed and
sequenced overlap cases; negotiation rounds and architect deadlock. All tests.

### Step 5 — Releases are project versions; deployments (D-059, D-060)

- Project versions: open → frozen; `based_on`; next release and hotfix creation; diff of two
  project versions; trace report.
- `environments`, `deploy_targets`, `deployers`, `deployments`, `deploy_logs`,
  `container_images` (deploy images), `has_deployment`, `uses`, `has_deployer`, `runs`,
  `deployed_to`, `built_for`.
- Each project gets a default deploy target `default` and, per environment, a deployer of kind
  `k8s_namespace` (today's behaviour: the environment's Kubernetes namespace in this cluster).
- Gates move from `in_environment` to deployments; PROD approval is recorded on the
  deployment; freezing happens on the first deployment beyond the lowest environment;
  rollback is a deployment of the previous succeeded project version.
- Builds: `builds` keyed by closure hash, `build_of` versions, `built_for` a target;
  `test_logs` with `logs_to` from test versions and builds.

Acceptance: promote a project version DEV → PROD with each gate refusing and passing;
deployment retry keeps every attempt in `deploy_logs`; a hotfix on a frozen PROD version
deploys while the next release stays open; the same project version deploys to two
environments through different deployers; rollback. All tests.

### Step 6 — Requirements, use cases, agents, schedules, audit (D-061, D-062)

- Requirements: content in `requirement_versions` (accepted / proposed / rejected — a
  proposed version is the human approval request), `tags`, `kind` (business, technical,
  decision…), `applies_to`, `verified_by`. Groups and missions removed; decisions become
  requirements of kind `decision`.
- Use cases: `relates_to`, `verified_by`.
- `agents`: per-project agent types with `prompt` and `permissions` (the actions — MCP tools —
  the type may use), seeded from the built-in defaults; the engine and the MCP server refuse
  tools outside the agent's permissions; the reviewer of a work item is never its author.
- `schedules` (project-scoped, `targets` edge), `audit_log`, `agent_chats` /
  `agent_chat_msgs` (`has_message`, `authored`, `refers_to`), `owns` edges.
- Approvals: the pending list is computed from the things awaiting approval.

Acceptance: requirement inheritance and counts; proposed requirement change approval; agent
permission refusal; schedule targets; audit entries for writes. All tests.

### Step 7 — UI

- API client and screens follow the renamed routes: work items with proposals (Work screen,
  Agents panel), project versions and deployments (Releases drawer), requirements with tags
  (Requirements screen), agents with prompts and permissions (Settings).
- The graph rendering (SystemMap, node shapes, overlays, badges) is unchanged.

Acceptance: `npm run typecheck`, unit tests and Playwright e2e pass.

### Step 8 — Wipe, deploy, verify

- Drop the `kodehub` database in the Kind cluster, rebuild images, deploy
  (`scripts/k8s.sh deploy`), run the smoke test, then `make check`.
- Manual end-to-end pass through the deployed UI and API: sign up, create a project, seed,
  plan, agent work on recorded responses, test now, deploy DEV → PROD, hotfix, backup and
  restore, Git export.

Acceptance: `make check` green; the manual pass is recorded in `docs/STATUS.md`.

## Progress

- [x] Ontology written (`docs/ontology.md`), decisions D-055 to D-062, this plan
- [x] Step 1 — shared collections (102 tests green; `o1_tenancy` covers isolation, foreign keys, fixed collection count, delete)
- [x] Step 2 — global renames and identity (`o2_remote_agents`; secrets carry `secures` edges)
- [x] Step 3 — version tree (`o3_version_tree`; `m1_graph_core`, runner `m2`/`m3`/`m8` on project versions)
- [x] Step 4 — work items replace changesets (`o4_work_item_proposals`, `m5_proposal_rules`; server `m4`, `m5_agents_demo`, `m5_guardrails`, `s7`, `s8` through `/api/work-items/{key}/proposal/…`)
- [x] Step 5 — project versions and deployments (`o5_deployments`; server `m7_deployments` replaces `m7_releases`: freeze on first promotion, ordered gates, prod approval, retry, rollback, hotfix through a proposal, diff, bisect along the version line)
- [x] Step 6 — requirements, use cases, agents, schedules, audit (`o6_governance_agents`; server `s3_c4_model`, `s9_git`)
- [x] Step 7 — UI (typecheck, unit tests and both Playwright specs green; graph rendering unchanged)
- [x] Step 8 — wipe, deploy, verify (Kind `kodehub` database dropped and re-created by the server on the v2 ontology: 74 collections, one named graph; `make check` green; manual pass recorded in `docs/STATUS.md`)
