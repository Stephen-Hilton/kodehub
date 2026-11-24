# Decision log

Record build-time decisions here: context, decision, consequence. Newest first.

---

## D-054 SeaweedFS as the self-hosted S3 tier (MinIO dropped)
- **Context:** MinIO's community edition is no longer a dependable open-source option (no
  published community binaries or images, admin features removed, community edition in
  maintenance). D-046 had already replaced it with our own `kodehub-storage`, but only because
  its images could not be pulled here, and left no self-hosted durable object store.
- **Decision:** Two roles, deliberately separate:
  - `kodehub-storage` stays the only object interface Kodehub components talk to (project-scoped
    keys, Kodehub tokens, local / cloud / hybrid modes, per-project backups). It does **not**
    grow into an object store (no replication, erasure coding or healing).
  - **SeaweedFS** (Apache 2.0, `chrislusf/seaweedfs:3.97`) is the optional self-hosted S3 store
    behind it, reached as an ordinary SigV4 upstream. Garage was set aside as AGPLv3 (the same
    concern as MinIO); RustFS as too young; Ceph RGW as too heavy for one cluster.
  - `deploy/k8s/optional/seaweedfs.yaml`: one pod in single-server mode with its S3 gateway on a
    ClusterIP service. `KODEHUB_STORAGE_BACKEND=seaweedfs scripts/k8s.sh deploy` applies it,
    generates its S3 identity into `.kode/k8s-secrets.env`, and switches storage to hybrid with
    SeaweedFS as the cold tier through the `storage-profile` ConfigMap and the `storage-upstream`
    Secret. The default stays `local`.
  - The storage service creates its upstream bucket at startup (S3 CreateBucket, "already
    exists" is fine, retried while the store starts).
  - `make check` runs `scripts/k8s.sh seaweed-check`: `kodehub-storage/tests/s5_seaweedfs.rs`
    against the in-cluster SeaweedFS (bucket creation, a wrong secret refused, cloud-mode
    objects, hybrid full + incremental backups, single-project restore). With the SeaweedFS
    profile, the smoke test also checks that a backup reaches the cold tier.
- **Consequence:** Moving to AWS S3, GCS (S3 interop) or another S3 store is a change to the
  profile values only. SeaweedFS's S3 coverage is partial, but Kodehub uses only put, get,
  delete, list and create-bucket.

## D-053 Sysadmin timer and the test budget
- **Context:** Spec §11.4 (a cluster manager agent wakes on an interval, sleeps when all is
  well, sets caps to fit the compute) and §11.5 (turn a test budget into a concrete plan; the
  daily run is always kept).
- **Decision:** Every `KODEHUB_SYSADMIN_SECS` (600) the orchestrator runs a deterministic check
  (CPUs, 1-minute load, agent work queued vs running). All well → nothing, no LLM call. Work held
  back by caps or an overloaded machine → one low-priority `sysadmin` work item carrying the
  metrics and current caps; the agent answers `set_caps`, written at app scope as a
  system-managed value (human hard limits are never written this way). The scheduler scales the
  component / container / context intervals by `demand / budget` when the estimated test time
  per day (average formal run time × runs per day) exceeds `test_budget_pct` of the cluster's
  CPU-seconds (`KODEHUB_CLUSTER_CPUS`, else this machine); the daily project run is outside the
  budget.
- **Consequence:** The test author agent can still override individual cadences with `schedule`.

## D-052 S11: speech transcription and remote MCP over OAuth
- **Context:** Spec §14.1 (spoken philosophy and requirements) and §13A.2 (remote MCP with the
  standard OAuth flow, each assistant its own agent user).
- **Decision:**
  - The UI records audio (MediaRecorder) and uploads it to `POST /api/transcribe`; a
    `transcribe` job (`kodehub/transcribe:dev`: faster-whisper with the `tiny.en` model baked in,
    ffmpeg for decoding any browser format) returns the text. The audio is deleted after
    transcription. No browser speech service is used, so Chrome and Tauri's WebKit behave the
    same; audio never leaves the cluster. A raw transcript becomes the philosophy with
    `raw_transcript` kept, and the kickoff chain's first architect step cleans it up.
  - PyAV is not used (PyAV 16 broke faster-whisper's call and 14.x has no wheel here); ffmpeg
    decodes to 16 kHz mono PCM handed to the model as an array.
  - Remote MCP: `POST /mcp` (JSON-RPC; batches; notifications get 202) reuses the stdio server's
    handler and calls the API with the **caller's own token**, so the MCP service adds no logic
    or security of its own. Unauthenticated calls get 401 with `WWW-Authenticate:
    Bearer resource_metadata=…`.
  - OAuth 2.1: RFC 9728 / 8414 discovery, RFC 7591 dynamic registration (public clients;
    redirect URIs https or loopback http), `/oauth/authorize` = a Kodehub sign-in + consent page
    (the human re-enters their password, which also defeats CSRF), PKCE S256 only, single-use
    codes (2 min), access tokens (1 h) that are ordinary PATs of an agent user named after the
    client and acting for the approving human, rotating refresh tokens (30 days, never accepted
    as bearer tokens). Codes and tokens are stored only as hashes. Revoking in Settings takes
    effect on the next call.
- **Consequence:** `make check` transcribes real speech (macOS `say`) in the cluster.

## D-051 S10: the UI — screens, in-node badges and overlays
- **Context:** Spec §8: keep the graph rendering; information inside nodes; overlays next to the
  element instead of side panels; new screens.
- **Decision:**
  - The Cytoscape map is untouched. An HTML layer (`NodeBadges`) follows Cytoscape's render,
    pan, zoom and layout events and draws small chips on each visible node: lifecycle dot,
    tests passed/total (rolled up; colour by state), business / technical requirement counts
    (own · with children), work in progress, candidate marker, build dot. Code-node chips sit
    just above the node so its label stays readable.
  - Clicking a chip opens a `Popover` beside it: tests (runs, history, *Test now*),
    requirements (own and inherited, grouped by the level they come from, with the philosophy as
    tiebreaker), lifecycle (explanation, versions, work), work in progress. Clicking a code
    node opens the former node panel in the same kind of floating card.
  - The left and right sidebars are gone. A top bar holds the project switcher and screens:
    Map, Requirements (philosophy editor, requirements list, where each attaches, push-down
    suggestions), Use cases (draft → checking → live, link expected-outcome tests), Work
    (approvals and external blockers first, the queue, spend, the agents' activity and
    changesets), Releases (the former drawer), Settings (layered values with their source,
    schedules, credentials with the subscription-token warning, external agents and tokens, Git
    bridge, backups), Projects (list and the create flow: name, sharing, philosophy typed or
    spoken, starting contexts, template).
  - The map stays mounted while other screens show (a remount lost the layout).
  - Templates: `master` (built-in contexts with best-practice requirements at the lowest level)
    or `project:<id>` (copy contexts, requirement groups, requirements and their edges — never
    code).
  - The map endpoint carries `test_counts`, `in_progress`, `lifecycle`, `requirements` and
    `candidates` per node; `/api/node-tests` backs the test overlay.
  - An architect item that fails its own retry limit now stops (blocked + a human notification)
    instead of retrying forever.
- **Consequence:** The splitter / resizable-panes e2e spec was retired with the panes; the doc 08
  demo spec runs on the new screens, and `v01.spec.ts` covers project creation, badges,
  overlays, settings and use cases.

## D-050 S9: Git import and export, database backups
- **Context:** Spec §13 and §3.4: a one-time Git import through an import container and an
  import agent, a Git export container on demand or on a schedule, and off-site backup of the
  database as well as storage.
- **Decision:**
  - `kodehub-worker git-import` (the build image: git, Rust, Python) clones shallowly and parses:
    Rust with `syn` (spans enabled, so signatures and bodies are sliced from the original source),
    Python with the standard `ast` module (run with `python3 -I`), every other language as one
    opaque module per file (`decomposed: false`). Units carry references (identifiers used).
  - The orchestrator stages the units in the project (`import_units`) and queues an `import`
    agent item; the agent answers with one `import_mapping` action (contexts, containers,
    components with unit-id prefixes). Applying it creates planned C4 nodes (the import was a
    human's request, so no approval round), assigns leftovers to "unmapped imported code", gives
    every component a generated requirement ("Preserve imported behaviour: <component>",
    satisfied by it), and opens an **import changeset** of new code versions that promotes
    without the test and review gates (there are no Kodehub tests yet) but with the orphan check.
    Opaque units stay staged on their component.
  - Re-importing a repository URL already imported in the project is refused (409): it would be a
    reverse sync, out of scope.
  - `kodehub-worker git-export` renders every language (tests included) and `KODEHUB.md` (the
    philosophy and the C4 tree, and a note that the repository is a mirror), fetches the branch to
    keep history, commits only when something changed, and pushes. Git credentials are layered
    (`git_token` credentials) and reach the container only as `GIT_TOKEN` in a per-job secret;
    they are spliced into https URLs and scrubbed from error messages.
  - Per-project graph backups: `export_project` dumps every collection of one project into
    `projects/<p>/backups/db-<ts>.json` in storage (so hybrid storage ships it off-site with the
    project's bundle); `restore_project` loads a dump into a new project. Schedules now carry an
    `action` (`test_run`, `backup`, `git_export`): the scheduler plans a backup every
    `db_backup_hours` (default 24) and a mirror export when `git_export` is configured.
- **Consequence:** Opaque (non-Rust/Python) code is visible as staged units but not yet code nodes.

## D-049 S8: agents as ephemeral tasks on the work queue
- **Context:** Spec §4, §10, §12, §14: one tiny agent base image, ephemeral stateless agents,
  per-type caps, escalation to an architect, external blockers to humans, credentials only for
  the duration of the work, metering and budgets, the kickoff chain, tests before code.
- **Decision:**
  - **Agent types** live in each project's `agent_types` collection (built-ins seeded: coding,
    test_author, review, planning, architect, import, design, sysadmin; custom types
    `custom:<name>` with stored instructions). Each has a prompt (`prompts/*.md`), a tool role
    (read-only graph tools) and an optional model.
  - **Dispatcher** (`AgentDispatcher` in `kodehub-server`, started by `serve()`): claims agent
    work items (caps and budget enforced by the claim), registers the intent, then:
    coding work goes through the existing changeset engine (plan → tests → code → blind review
    → formal tests → promote); every other role is **one** agent task answering
    `{change_log, actions}`, applied through the store by `actions.rs` (create_nodes, enqueue
    with `after` dependencies, propose_requirement_change, set_philosophy (architect only),
    write_tests, schedule, set_caps (sysadmin only), external_blocker, use_case_verdict,
    import_mapping).
  - Requirement edits now **enqueue** a high-priority coding item instead of calling the engine.
    Plans approve themselves unless `human_engagement = plans` (a new value that keeps the
    v0.05 human plan approval; the M5 demo and the UI e2e set it). An item waiting on a human
    plan approval is `waiting_human`; it settles when its changeset ends.
  - **Approvals**: new contexts (and containers under `human_engagement = containers`) and every
    requirement change become `approvals` a human decides (`POST /api/approvals/{k}/decide`,
    human session only); approved proposals are applied.
  - **External blockers** (no credential, a revoked key …) mark the item `blocked` (no attempt
    counted) and create a notification flagged `unrelated_to_code`; `POST /api/work-items/unblock`
    requeues after a human fixed it.
  - **Tests before code**: `write_tests` opens a `tests_first` changeset of new tests only, which
    promotes without passing them (they fail until the code exists) and without a code review;
    orphan checks still apply. Tests are excluded from lifecycle roll-up, so a component holding
    only tests stays `planned` (and is not scheduled).
  - **Kickoff chain** on project creation (setting `kickoff`, default on): architect
    `cleanup_philosophy` (only for a raw transcript) → architect `evaluate_requirements` →
    one planning item per context; planning agents enqueue the next levels themselves (per
    container, then per component a test_author item before a coding item).
  - **Use cases** going live wait in `checking` for an architect's `use_case_verdict` (setting
    `use_case_check`, default on).
  - **Each agent task** gets a short-lived, read-only internal Kodehub token for its graph tools
    (`KODEHUB_TOKEN` for `kodehub mcp`, bearer for the API backend's tool calls), and its usage
    is recorded per project and work item.
  - **Credentials and backends**: the project's `agent_backend` (when set) or the server config.
    `api` uses only a stored API key (or the legacy `KODEHUB_ANTHROPIC_API_KEY`); `claude_code`
    uses only the owner's stored subscription token, handed to that one `claude` process as
    `CLAUDE_CODE_OAUTH_TOKEN`, or the developer's CLI login on a laptop. Missing credentials are
    an external blocker, never a fallback to the other kind.
  - **Agent containers**: in the cluster (or with `KODEHUB_AGENT_CONTAINERS=always`) every task
    runs as `kodehub-worker agent` in `kodehub/agent:dev` through `ContainerBackend`. The model
    credential goes into a per-job Kubernetes Secret deleted with the job; the task input
    (holding the Kodehub token) is deleted from storage afterwards. The image is the core test
    layer + the `claude` CLI's standalone native binary (Node only in the builder stage) + the
    kodehub CLI; ~720 MB, almost all of it the `claude` binary.
- **Consequence:** The smoke test runs an agent task in its own Kubernetes Job (recorded
  backend, no credential). Live agents in the cluster need a stored credential.

## D-048 S7: build and test containers, shell tests, formal runs
- **Context:** Spec §5.3–5.7: build once (deterministic, no LLM), test many times from the
  artifact, tests are plain shell scripts, only formal runs count, baseline blame, failures
  become rework.
- **Decision:**
  - A **test bundle** is the unit a build container produces: the tests of one language, keyed by
    language + every (test, version, closure hash) + renderer + toolchain. Rust: the crate's
    test binary from `cargo test --no-run`, checked with `--list` so a missing test can never
    pass. Python: the rendered package with dependencies and pytest vendored. Every test gets
    `tests/<fn>.sh`; its exit status is the result (one test per process, so a test's output
    cannot forge a pass). A test whose version has `attributes.shell = true` is an authored
    shell script run next to the language artifact.
  - Bundles live in the storage service at `projects/<p>/artifacts/<key>/<lang>-test.tar`; an
    `artifacts` node records it with `built_from` edges to the test versions. An existing
    bundle is never rebuilt.
  - Containers never write the graph: the orchestrator puts the job input in storage, runs
    `kodehub-worker <build|test> --input … --output …`, and reads the output. Runtimes:
    `kubernetes` (a `batch/v1` Job through the Kubernetes API with the pod's service account;
    no kubectl) and `local` (the same entrypoint as a subprocess, used by `make dev` and
    `cargo test`). Storage falls back to `file://$KODEHUB_HOME/objects` off-cluster.
  - Test container types are images layered on one core image (`kodehub/test-core` → `-rust`,
    `-python`; `-browser` adds Chromium and is built on demand with
    `scripts/k8s.sh images-browser`). A bundle names the type it needs.
  - The orchestrator loop (in `kodehub-server`, stateless, `KODEHUB_WORKER=off` disables it)
    claims `test_run` items, ensures bundles, runs them, and records `formal: true` runs. A
    failing test whose previous formal result was not a failure is new: it gets one
    high-priority `fix_failure` coding item. Coverage, lifecycle and merge checks count only
    formal runs (`formal != false`); agents' pre-submission runs will be `formal: false`.
  - Merge checks compare failures with the stable graph's latest formal result for the same
    test: already failing there → `preexisting_failures`, not blamed and not blocking.
  - PyO3 extension tests stay on the in-process path (not bundled) in the pilot.
- **Consequence:** `make check`'s smoke test now drives the full chain in Kind: sign-up, project,
  seed, *Test now* → scheduler → orchestrator → build Job → test Jobs → formal results.

## D-047 S6: one claim-and-lease protocol, work queue, scheduler, intents
- **Context:** Spec §3.3, §4.4, §4.5, §5.7, §10, §11.3, §12.2.
- **Decision:**
  - The claim step is a single AQL `UPDATE … OPTIONS {ignoreRevs: false}` filtered on the
    document's `_rev` and claimability; a competing replica gets a revision conflict (1200) and
    skips. Leases expire (`lease_expires`); expired claims are claimable again. The same protocol
    claims work items, schedules and triggers.
  - Work items, schedules, triggers and usage are **global** collections with a `project` field,
    so one query serves every project and every replica. Intents and negotiations live in the
    project's own collections.
  - Caps: per project and agent type from `agent_caps`, plus an optional
    `agent_caps_hard_limit`; container work (`build`, `test_run`, `git_*`) is uncapped. A project
    at its `budget_usd` claims nothing new; running items finish.
  - Failure: the item is closed and a fresh high-priority item carries the full history. When
    `retry_count` reaches `retry_limit` (default 3) the follow-up is an architect item
    (`resolve_hard_failure`). Combined (absorbed) items follow the retry and count once.
  - Overlap = Jaccard over intent nodes; ≥ `overlap_absorb_threshold` absorbs into the older
    item, smaller overlap re-queues the newer item behind the older (`depends_on`), and a
    shared-end-state intent opens a negotiation; after `negotiation_rounds` per side without
    agreement an architect item decides.
  - `kodehub-scheduler` is its own crate and container (two replicas, 16 MiB request). Each tick
    it rebuilds the default cadence from `test_cadence` and node lifecycles (only ready-for-test
    or live nodes; the daily project run always, and it cannot be disabled), then fires due
    schedules and triggers as `test_run` work items. Offsets per node spread runs over the
    interval; `next_due` = actual run + interval. A merge records a `version_landed` trigger per
    promoted symbol; *Test now* records a `test_now` trigger.
- **Consequence:** `test_run` items queue up until S7's orchestrator worker runs them in test
  containers.

## D-046 S5: Kind, one server-side image, the storage service
- **Context:** Spec §3: everything on Kubernetes (Kind on a laptop), including the test
  framework; a storage service with local / cloud / hybrid modes and per-project backups.
- **Decision:**
  - Kind cluster `kodehub` (`deploy/kind/cluster.yaml`), host ports 18529 (ArangoDB), 17878
    (server + UI), 19000 (storage) — 8529/7878 are often taken on a developer machine. The
    developer's current kubectl context is restored after cluster creation.
  - Images are built locally and imported into the node with `ctr images import` (`kind load`
    fails on single-platform pulls of multi-arch images). Pilot: **one** server-side image
    `kodehub/kodehub:dev` holds every binary (server, scheduler, storage, worker, CLI) plus the
    Rust and Python toolchains; the agent image (S8) and the test images (S7) are separate.
  - Secrets (ArangoDB password, `KODEHUB_SECRET_KEY`, storage token) are generated once into
    `.kode/k8s-secrets.env` (git-ignored) and applied as the `kodehub-secrets` Secret.
  - `make check` now runs integration tests against ArangoDB **in Kind** and ends with
    `k8s-check` (image build, deploy, smoke: health, a storage round trip, UI served, API
    requires a token, scheduler ready). `make dev` keeps a host server against the Kind DB for a
    fast loop; `make up` runs everything in the cluster.
  - The MinIO images are not pullable here (and MinIO is dropped for good: D-054), so
    `kodehub-storage` serves S3 path-style objects itself (PUT/GET/HEAD/DELETE, ListObjectsV2 XML) with a bearer token instead of SigV4.
    `cloud` and `hybrid` use an upstream that is real S3 (SigV4 presigned requests via
    `rusty-s3`) or another `kodehub-storage` (bearer). Backups: per project (objects under
    `projects/<p>/`), first a full tar bundle, then incremental bundles with deletions;
    restore applies from the last full bundle and touches only that project.
  - `/health` is exempt from the Host check (kubelet probes by pod IP); in-cluster service names
    are allowed through `KODEHUB_ALLOWED_HOSTS`.
- **Consequence:** `docker-compose.yml` is obsolete but still in the tree (a tool safety check
  blocked its removal; delete it by hand). The ArangoDB database's own off-site backup is S9.

## D-045 S4: releases select versions and move through environments
- **Context:** Spec §16 (as amended in D-042): releases support versions at every level, select
  them by rule, are open in DEV and immutable once cut, move one environment at a time, can be
  cloned downward, and get patch releases for bugs found after DEV.
- **Decision:**
  - `supports` edges (version → release, `{node}`; unique per release and node) replace
    `included_in`. The release document still keeps a graph + trace snapshot so gates, runs and
    bisect are reproducible; `manifest_hash` identifies the content.
  - Selection rule (stored on the release): `stable` (default) | `newest` | `as_of` | `explicit`,
    with `filter` (`tags_any`, `tags_all`, `attributes` matched on the node, then the version,
    `levels`, `nodes`) and `pins`. Filtering code keeps its build closure
    (`include_dependencies`, default true). Edges of a selected version that differs from the
    working graph are re-inferred from its own code.
  - Release keys: `r<N>` (per project). Patch releases: `<source>.<n>` (`r456.1`).
  - States: `open` (in the lowest environment; reselect / swap / changesets naming it) → `cut`
    when it first enters a higher environment → `retired` when replaced. A clone into a higher
    environment is `staged` until it passes that environment's gate.
  - Environments come from the project's `environments` setting (default dev, test, qa,
    preprod, prod; preprod and prod `max_releases: 1`, prod needs a human). `in_environment`
    edges (release → environment, `validFrom`/`validTo`) replace `currently_running`,
    `previously_ran` and `deployed_to`. Sparse unique indexes on `current_release` and `slot`
    enforce hard rule 5 in the database; a full environment retires its longest-standing
    occupant on entry.
  - Gates by environment: test = build; qa = all tests, trace report, live use cases covered;
    preprod = smoke run; prod = smoke run + one-time human approval (only humans get one).
    Promotion is ordered; patch releases and rollback run the gate of the environment they enter.
  - Patch approvals are bound to the release being patched (the patch does not exist yet).
  - Rollback re-enters the most recently retired release through the gate and marks both sides
    so a second rollback walks further back instead of bouncing.
- **Consequence:** The v0.05 `deploy`/`rollback` store calls are gone; the environment agent owns
  gating. The UI derives the pipeline from the environments list.

## D-044 S3: SCD versions at every level, philosophy root, lifecycle
- **Context:** Spec §5.2 wants node → version edges with `validFrom`/`validTo` at every C4 level,
  candidates that never disturb the live version, plus requirement kinds, inheritance, a
  philosophy node, use cases and lifecycle states.
- **Decision:**
  - One edge collection, `has_version` (node → version), replaces `current_version` and
    `previous_version`. Fields: `seq` (per-node version number), `state` (candidate | stable |
    retired | discarded), `validFrom`, `validTo`, `changeset`. Invariant 1 is enforced by the
    database: `stable_of` is set only on the stable edge and carries a **sparse unique index**.
    Promotion closes the old edge and opens the new one with the same timestamp, in one
    transaction; `check_invariants` also verifies that intervals never overlap.
  - Code versions stay content-addressed in `versions`; context/container/component definitions
    (title, description, technology, tags, attributes, …) are hashed into immutable
    `c4_versions`. Every generic write of a C4 node syncs its version; a no-op write makes none.
  - Candidates: proposing a version for an existing symbol adds a candidate edge (no
    `validFrom`); withdraw/reject marks it `discarded`; merge promotes it. New symbols still wait
    for merge (D-023).
  - Changeset state names are kept (draft, planned, approved, building, reviewing, testing,
    ready, merged, rejected, escalated) and mapped onto spec §5.8 (open, submitted, …, promoted)
    rather than renamed through the engine, UI and recordings. The review gate honours the
    `blind_review` setting; `MergeChecks.review_required` records it.
  - `philosophy/philosophy` is the single philosophy node and the **root of the C4 graph**
    (`contains` → contexts); project-level requirements and `traces_to` point at it. It keeps its
    revision history. Missions are retired: `/api/missions` and agent context read the
    philosophy. The seed moved from `missions.yaml` to `philosophy.yaml`.
  - Requirements carry `kind` (default `technical`; any lower-case word, so the list can grow).
    `requirements_for(node)` walks `contains`/`member_of` up to the philosophy and merges own and
    inherited requirements (groups expanded), nearest first. Per-node counts report own and
    own-plus-children by kind; the map carries them and the lifecycle on every node.
  - Lifecycle is derived: code is planned (no stable version), in_build (candidate, or an explicit
    rework flag), ready_for_test (stable with tests), live (stable and its tests pass). Stable code
    without tests is in_build: spec §14.4 requires tests before "ready for test". Parents roll up
    (all planned → planned; any not yet built → in_build; else the least advanced state).
  - Use cases (`use_cases`, `relates_to` → requirements, `expected_by` → tests) start in draft;
    going live needs at least one expected-outcome test. Containerized agents cannot change the
    state.
- **Consequence:** Release manifests still use the v0.05 edges until S4. Re-seeding over a changed
  graph works (requirement `kind` is editable).

## D-043 S2: tenancy, identity and the auth layer
- **Context:** v0.05 had one server-wide "active project" and a per-launch UI secret. v0.1 needs
  users, orgs, sharing and per-call permissions (spec §6, §11, §13A).
- **Decision:**
  - Per-project collection sets are kept (they already make "every node belongs to one project"
    structural). The project is chosen **per request** (`x-kodehub-project` header or `?project=`,
    else the caller's default) and applied through a tokio task-local that the Arango handle reads
    before its shared project. `spawn_scoped` carries it into background tasks; WebSocket events
    are tagged with their project and delivered only to subscribers of that project.
  - Identity, settings, credentials and audit are global collections (`kodehub_*`). Ownership and
    permissions are edges: `kodehub_member_of`, `kodehub_access`, `kodehub_acts_for`. Org roles map
    to project permissions: admin → admin, member → write.
  - Passwords: argon2. Sessions (`khs_`), PATs (`khp_`) and internal agent tokens (`khi_`) are
    random 256-bit tokens stored only as BLAKE3 hashes. PATs are issued only to **agent users**
    (spec §13A.1: an external assistant never acts as the human); humans use sessions.
  - Human-only actions keep their v0.05 handler checks: the auth layer strips any client
    `x-kodehub-ui` header and sets it only for a human session. The UI secret file, meta tag and
    Tauri command are gone.
  - The first registered human is the system admin and inherits ownerless projects (the project a
    fresh database starts with). Registration is open by default (`open_registration`, app scope).
  - Credentials: ChaCha20-Poly1305 with `KODEHUB_SECRET_KEY` or `$KODEHUB_HOME/secret.key`. The
    `claude_code` backend may use only a subscription token of the owner of a user-owned project;
    the `api` backend only API keys. Format checks refuse an API key stored as a subscription
    token and vice versa.
  - Every non-GET API call is audited (method + path, actor, actor class, on-behalf-of, project,
    status). Request bodies are not recorded; detail values are scrubbed of secrets.
- **Consequence:** All HTTP tests authenticate: `req()` is an external agent with a PAT, `ui()` is
  a human session. The CLI/MCP read `KODEHUB_TOKEN` / `KODEHUB_PROJECT`. Fixed on the way: list
  filters no longer treat `?project=`/`?token=` as field filters.

## D-042 Releases select versions; patch releases; clone downward only
- **Context:** The author wants several releases per environment, fixes for bugs found after DEV
  without restarting from DEV, cloning, and releases that pick code by rule. Editing a release
  after it is cut would break hard rule 4.
- **Decision:**
  - There is no separate "live line" that changes land on. Every change is a new numbered version
    node. Version state on its node is candidate / **stable** / retired (renamed from "live" so it
    is not confused with environments or the node lifecycle "live"). Release membership is only the
    release's `supports` edges.
  - A release picks versions by a recorded selection rule: newest, stable, as of a timestamp (read
    from the SCD intervals), or explicit, with per-node pins. Open while in DEV, cut (immutable)
    when first promoted out of DEV.
  - Clone into the same or a lower environment only (DEV < TEST < QA < PRE-PROD < PROD).
  - A bug found after DEV: a normal changeset makes and tests the fix; the orchestrator makes a
    patch release (`r456` → `r456.1`, cloned at the same environment, with one `supports` edge
    swapped) and runs only that environment's gate. The original is retired and unchanged. This is
    also the PROD hotfix path.
  - `max_releases` per environment: unlimited for DEV/TEST/QA, 1 for PRE-PROD/PROD.
- **Consequence:** Hard rules 1, 4 and 5 reworded. Changesets name the open releases they join.
  Spec 5.2, 5.8, 14.4 and 16 updated.

## D-041 v0.1 spec adopted; review answers folded into it
- **Context:** The v0.1 layered spec replaces the v0.05 pilot rules in several places (multi-user
  auth, Kubernetes, stored credentials, per-node versions). A review raised conflicts and gaps;
  the author answered them on 2026-11-23.
- **Decision:** The v0.1 spec governs; `CLAUDE.md` was rewritten to match and docs 00–08 carry a
  "superseded where it conflicts" banner. Folded into the spec:
  - Versions at every C4 level use type 2 SCD edges (`validFrom`/`validTo`); artifacts are separate
    nodes keyed by `closure_hash` (spec 5.2).
  - Changesets group candidates and promote all-or-nothing (5.8). Blind review stays as a review
    agent behind the `blind_review` project setting, on by default (5.6).
  - Test container types are per-project graph nodes built as layers on one core image (5.4.1).
  - Git import = deterministic parse container + import agent that proposes the C4 mapping (13.1).
  - Releases are immutable sets of `supports` edges to versions; a release is in one environment
    at a time; no release branches (16).
  - Org projects fall through to app defaults, not contributing users; per-actor settings come
    from the acting user (11.2). Subscription tokens: user level on user-owned projects only.
  - Overlap = Jaccard, absorb ≥ 0.6, 3 negotiation rounds. Daily full run is outside the test
    budget. Speech: audio upload + local Whisper container. Auth: local accounts + sessions + PATs
    first, MCP OAuth last.
  - The architect agent only makes higher-level decisions; the test author agent reviews test
    quality.
  - No v0.05 data migration; the seed project is re-created.
- **Consequence:** `make check` will require a Kind cluster from rollout step 3. Docs 01–08 stay as
  background until the audit (`docs/09-v0.1-audit-and-plan.md`) rewrites what is still needed.

## D-040 Project renamed from "Lattice" to "Kodehub"
- **Context:** The project and its root folder were renamed.
- **Decision:** Every identifier, crate (`kodehub-*`), env var (`KODEHUB_*`), MCP server name,
  Docker resource and doc now says kodehub. The data dir is `.kode` (it was `.lattice`). The
  render domain strings changed, so `GENERATOR` went to `kodehub-render/4` with a new golden. The
  closure-hash domain tag is now `kodehub-closure-v1`.
- **Consequence:** Content and closure hashes from before the rename do not match new ones. The
  old ArangoDB volume was removed; reseed with `make seed`.

## D-039 Hardening from the final critical review (`docs/reviews/review-final.md`)
The UI was re-scored 7.5/10. M5 and M8 were "partially" accepted over trust-boundary gaps. Changes:
- **#8 Human-only actions need a per-launch UI secret, not an Origin header.** The server
  generates a random secret at start and writes it to `$KODEHUB_HOME/ui-token` (0600, and denied to
  sandboxed generated code). It reaches the UI as a `<meta>` tag injected into the served
  `index.html`, through a Vite dev plugin, or through a Tauri command. The UI sends it as
  `x-kodehub-ui` (or `?ui=` for the terminal WebSocket). Every human-only route requires it:
  attested reviews, plan approval, transitions, escalation resolve, prod approvals (now UUIDs),
  chat Apply, requirement create/edit/edge cases/examples, manual edit, rollback, settings and the
  terminal. MCP has no requirement-editing tools anymore; agents propose through the
  orchestrator chat and a human applies the proposal. Tests assert that a forged Origin without
  the secret gets 403.
  - **Residual risk, accepted for the pilot ("single user, local"):** a process running as the
    same OS user (for example a shell in the embedded terminal) can still read the token file or
    scrape the served page. The secret stops agents that work through MCP/HTTP. It does not stop
    a hostile local user, which is out of the pilot's threat model.
- **#9 No code or trace edits outside changesets.** The HTTP edge API refuses `calls`,
  `depends_on`, `member_of`, `tested_by`, `binds_to`, `verifies` and `satisfies` (use
  `propose_edge`). A symbol `visibility` patch refreshes closure hashes under the write lock. Seed
  import takes the write lock.
- **#7 Requirement-centric coverage.** Requirements carry `covered_revision`. Any merged
  changeset that lists a requirement whose `revision > covered_revision` must include a verifying
  test, whatever opened it (manual edits included). The merge then raises `covered_revision` to
  the revision it addressed. `at_risk = revision > covered_revision` is derived, so overlapping
  changesets can no longer leave it stuck amber. Seeded requirements arrive covered, and new ones
  start at 0. (M4's HTTP scenario now proves the manual-edit false green is blocked.)
- **#1, #2, #5 PyO3.** Every extension builds in its own target dir
  (`cargo-target/ext/<closure16>`), so the shared-path overwrite class is gone. Only deterministic
  compile errors are cached. Python artifacts that embed an extension key on both toolchains.
- **#6** Namespace re-exports are part of the library build cache key.
- **#3** `binds_to` targets must be runnable functions (clear render error). The shim uses the
  pinned/bound version. A Python symbol shadowing a bound name is refused.
- **#10** The `→ qa` gate also refuses requirements that are at risk or unverified in the release.
  `cut_release` takes the write lock (graph and trace snapshot are consistent).
- **#11** Rework rounds that *modify* an existing symbol outside the plan go back for
  re-approval. New helpers stay allowed but flagged.
- **#12** The coder's test ban applies after name resolution. `propose_version` refuses a kind
  mismatch with an existing symbol.
- **#13** A rolled-away-from release is skipped only until it is deployed again.
- **#14** The generator golden test now covers the PyO3 extension, the shim and namespaces
  (`kodehub-render/3`).
- **#15** The recording has a third cycle (save-as-example → tester writes the test → merge), so
  the e2e demo ends **green**.
- **New tests:** `m5_guardrails.rs` (test-rewrite refusal via name lookup, out-of-plan
  re-approval, `covered_revision`), forged-origin and edge-API refusals in M4, diff `reasons` in M7,
  M8 extension/cache-hit assertions.
- **UI:**
  - `overflow: clip` on the shell (no sideways scroll).
  - Toasts over the map's bottom-left, clear of the sidebar actions.
  - The requirement editor keeps the map visible and lights the edited requirement's path, with
    multi-line input/expected fields.
  - A "⇣ last result" paste button on string fields (step 8 is now hash → one click → run).
  - Longer timeline labels, and the toolbar above banners.
- **Not done:** an e2e for escalation and chat Apply (the recorded backend has no chat cycle).
  Laptop-size screenshots should be regenerated.

---

## D-038 Namespaces are applied to library builds
- **Decision:** `namespaces` + `exposes {public_name}` edges (the projection layer) are loaded for
  every library build, including release gates. In Rust they render as
  `pub mod <name> { pub use super::<symbol> as <public_name>; }`, and in Python as
  `kodehub_gen/<name>.py` with `from . import <symbol> as <public_name>`. The seed ships an
  `auth_sdk` namespace (`check_login`, `hash_new_password`, `sign_in`), tested in M2.
- **Consequence:** A public SDK surface can be renamed without touching code symbols. Run-now and
  test builds don't include namespaces because they aren't needed there.

---

## D-037 UI changes from the M6 review (UI scored 6.5/10)
- **Map legibility:** the layout is aspect-aware. Each container lays its components out across
  or down, whichever better fits the map pane, so the doc 08 map fills the pane with readable
  labels. Cytoscape's `min-zoomed-font-size` hides unreadable text.
- **Proposals:** *modified* symbols get a dashed violet border and a `Δ` prefix, and new symbols
  are dashed ghosts. "Untested" has its own dotted style and legend entry, distinct from "at
  risk". The planted duplicate fixture is hidden with the tests toggle.
- **Node panel:** a docked column, not an overlay, so it never hides the node you clicked. The
  map resizes and re-centres the selection.
- **Run now:** "Fill from a requirement example", remembered arguments per symbol, and a Copy
  button on results. Arguments are initialised once per symbol, so a late load can never clobber
  typed input. That StrictMode race is what made the e2e's first hash run send an empty password.
- **Time slider:** stops are labelled. The seed stop marks the *end* of the import. History mode
  shows structure with neutral colours plus "changed since then", and no ghosts.
- **Agents:** each agent shows a to-do list for the active changeset. Cards show confidence flags
  on their face (review verdict, blocking findings open or resolved, tests, decisions, out of
  plan). Escalated changesets have **Send back to coder** and **Accept & re-test** actions, and
  the human's call is recorded as a Decision. Chat proposals need **Apply** or **Discard**. The
  recorded backend shows a "demo replay" badge.
- **Toasts:** bottom-right, at most three, deduplicated, about 4.5 s.
- **e2e step 8** now proves the locked path honestly. It hashes a real password in the app,
  shows the unlocked user authenticates, then that locking the same user returns
  `invalid_credentials` from the cache.

## D-036 Hardening from the M5–M7 critical review (`docs/reviews/review-m6.md`)
All 22 findings were accepted. Implemented:
- **#1 Green means covered.** A changeset records the requirement revisions it `addresses` (set
  when the plan is attached). Merge clears `at_risk` only if the revision still matches and no
  other open changeset works on the requirement. Requirement-edit changesets must include a
  proposed test that `verifies` each requirement (`requirements_covered` merge check).
- **#2** The generic transition route only allows approved, building, reviewing, rejected and
  escalated. `merged` happens only inside `merge_changeset`. Human-driven flows use
  `/changesets/{k}/test` (which advances to `ready` when the tests pass and the review counts)
  and `POST /api/manual-edit`.
- **#3, #12** A process-wide write lock serialises merges, deploys and rollbacks. Merge re-reads
  the changeset inside the transaction (a concurrent reject wins) and writes history from that
  copy.
- **#4 Attested reviews.** Each review stores a server-set `source`: `engine_reviewer` or
  `human` (only from an allowed browser Origin) count, `api` (MCP/CLI) is advisory. Every blocking
  finding from *any* round must end resolved. `submit_review` is documented as advisory.
- **#5** The coder may not change test symbols, and such changes are refused and logged. The
  reviewer sees `MODIFIED TEST` old/new diffs and *every affected test*, not only the
  requirement's own.
- **#6** Releases snapshot their trace edges (satisfies, verifies, contains, includes, requirement
  revisions). The `→ qa` gate evaluates failing requirements and orphans on the release itself
  (`release_trace_report`).
- **#7** The prod smoke run re-executes every test (`force`). Human sign-off is a one-time,
  release-bound approval id issued only to the UI. `approved_by` strings are ignored and removed
  from the MCP schema.
- **#8** Rollback walks back. A release that was rolled away from is never chosen again.
- **#9** Escalated changesets can be resolved by a human. If a `ready` changeset finds its tests
  stale at merge time, the engine goes back to `testing`.
- **#10** Pinned versions on touched symbols' edges survive overlay and merge.
- **#11** A merge fails if a "new" symbol was created by another changeset in the meantime.
- **#13** Round-0 coder changes outside the approved plan send the changeset back to `planned` for
  re-approval, flagged `out_of_plan`.
- **#14** The e2e step 8 was fixed (see D-037).
- **#15** An exhausted recording fails loudly. Re-selecting the recorded backend resets it.
- **#16** The origin allowlist is exact: the Tauri origins, the Vite/e2e ports, the server's own
  origin and `KODEHUB_UI_ORIGINS`. Requests whose `Host` isn't loopback are refused (DNS
  rebinding), and the terminal WebSocket requires an allowed browser Origin.
- **#17** Release diffs also report renames, visibility changes and dependency changes, each
  with a `reasons` list.
- **#18** Agent prompts go to `claude -p` on stdin, never argv (verified live).
- **#19** The orchestrator's chat actions are proposals. A human must click Apply, attributed
  "human (proposed by orchestrator)".
- **#20** Still open: the API-backend live demo needs a Console key (D-030).
- **Found while fixing #14 — a cache-key bug of my own:** a renderer change did not bump
  `GENERATOR`, so artifacts built under the old renderer were reused. Among them was a binary
  produced when the shared `kodehub_run` path could be overwritten across processes. The
  version is now `kodehub-render/2`, and a golden test (`renderer_changes_bump_the_generator_version`)
  fails whenever rendered output changes without a version bump.
- **Live re-run** after these changes (real Claude Code agents, `DEV3_TOKEN`): plan, 5 tests from
  the requirement, the blind reviewer found the timing leak in *both* the Rust and Python paths,
  one rework, 2 Decisions recorded, 13/13 affected tests pass, merged. About 2 minutes.

---

## D-035 Near-duplicate detection without embeddings
- **Context:** Doc 02 suggests embedding version bodies and flagging similar pairs (milestone 5+).
- **Decision:** `GET /api/duplicates?near=0.6` adds near duplicates: symbol pairs of the same
  language and kind whose current signature + body have Jaccard similarity ≥ the threshold over
  token 3-grams (comments and strings stripped). It is deterministic, offline and free, and
  easy to swap for embeddings later behind the same endpoint. Exact duplicates (same content hash)
  are reported separately.
- **Consequence:** The orchestrator and the UI can show merge candidates. The merge guardrail
  (split when option branches share less than half their logic) is left to a later phase.

---

## D-034 M8: PyO3 binding through a generated extension (no maturin)
- **Decision:** A Python symbol with `binds_to` edges to Rust symbols gets a generated Rust crate
  (`python-ext` target): the bound Rust closure, plus a `#[pyfunction]` per bound function, in a
  `cdylib` named `kodehub_ext` (PyO3 0.30, `abi3-py311`, macOS `-undefined dynamic_lookup`). The
  wrapper takes and returns **JSON** (`serde`), so any run-now-compatible Rust type crosses the
  boundary without hand-written conversions. The generated Python package exposes each bound
  function under its Rust name via a shim that calls the extension. The runner builds it with
  cargo (fetch, then a sandboxed offline build, `PYO3_PYTHON` = the venv) and drops
  `kodehub_ext.so` into the package. `maturin` is unnecessary for an in-place extension.
- **Consequence:** `binds_to` is a build dependency, so a Python wrapper's closure hash includes
  the Rust code. A Rust signature change therefore makes the wrapper's tests stale (at risk), and
  impact analysis follows the binding (tested in `m8_pyo3_binding`).

## D-033 UI design choices
- **Deterministic architecture layout:** force-directed layout (fcose) wasted space inside nested
  compounds and made labels unreadable. The map uses a deterministic C4 layout
  (`mapModel.computeLayout`, unit-tested): components side by side with callers first, and inside
  each component one band per language with layered columns by dependency depth (tests and
  callers left, types right). Proposed (ghost) nodes get their own trailing column.
- **Semantic zoom:** depth follows zoom *relative to the fitted zoom*. At fit you see code, and
  zooming out collapses to components, then containers, then contexts. Double-click expands one
  compound. Programmatic fits never change the depth.
- **Tests:** test symbols are hidden on the map by default to keep it legible (one-click toggle).
  They are always listed in the node panel and the requirement editor. Ghost nodes are always
  shown.
- **Manual edits:** a text-view edit opens a changeset, proposes the version, moves it straight to
  `reviewing` and resumes the engine. It gets the same blind review and tests as agent work.
- **Look:** a dark "mission control" theme with tokens for the health colours (green, amber, red,
  grey) and violet for proposals.

## D-031 Symbol health counts the tests that reach the symbol
- **Context:** C4 health is "all tests beneath pass". Applied to symbols, that left types and
  helpers (`User`, `AuthResult`) amber "untested" even though every R1 test exercises them.
- **Decision:** A symbol's health set is `tested_by` tests beneath it **plus every test whose
  closure reaches it** (inbound calls, depends_on, member_of and binds_to). C4 nodes keep "tests
  beneath".

---

## D-030 Live acceptance runs (M4, M5)
- **Claude Code backend:** `m4_claude_mcp` (Claude Code lists requirements and runs coverage
  over MCP) and `m5_live_claude_code` (the full doc 08 demo with real orchestrator, tester,
  coder and blind reviewer agents) both pass using only a long-lived Claude Code token chosen by
  env var name, with `ANTHROPIC_API_KEY` unset. The 2026-11-05 run on `claude-opus-5-5`: the
  reviewer requested changes, the coder reworked once, every affected test passed, the merge
  happened and coverage went green, in about 2 minutes. Run with `make live-test`.
- **API backend:** no `KODEHUB_ANTHROPIC_API_KEY` was available on the build machine. The backend
  is covered by a mock Messages API test (headers, `anthropic-version: 2026-10-21`, tool loop,
  structured output, narrow tool set, refusal of `sk-ant-oat` tokens). The same live demo runs
  on it with `KODEHUB_LIVE_DEMO=1 KODEHUB_LIVE_BACKEND=api`.
- **Consequence:** the "demo passes on the API backend" criterion is implemented but not executed
  live. That needs a Console key.

## D-029 M7's backend was built before M6's UI
- **Context:** M6's acceptance ("perform the doc 08 demo entirely in the app") includes step 9,
  cut a release and promote it to `test`, which needs M7.
- **Decision:** I built the release / environment backend and its acceptance tests (`m7_releases`)
  before the UI, then built the UI on top of both. The order of milestones is unchanged in every
  other respect.

## D-028 Environment agents are deterministic gate checkers
- **Decision:** `EnvironmentAgent` checks these gates in code and records every check on the
  `deployed_to` edge:
  - `→ test`: the release builds for Rust and Python.
  - `→ qa`: every test in the release passes, no requirement fails and no release symbol is an
    orphan.
  - `→ prod`: the QA smoke run of the release tests passes and `approved_by: human` is given.
  Promotion must follow dev → test → qa → prod. A release enters dev when it is cut. Each
  simulated environment gets `$KODEHUB_HOME/envs/<env>/config.toml`, and run now in `test`, `qa`
  or `prod` executes the release's own code graph (stored in the immutable release document). Bisect
  binary-searches releases by cut time, running tests on each release graph (cached by closure
  hash).
- **Consequence:** Gates are reproducible and free. An LLM environment agent could summarise gate
  reports later.

## D-027 Hardening from the M0–M4 critical review (`docs/reviews/review-m4.md`)
The headless review agent (claude-opus-5-5, `DEV2_TOKEN`) raised 2 critical, 6 high and 12 medium
findings. Accepted and implemented:
- **Secrets never reach generated code (critical).** Every child process (builds, tests, run now,
  pip) starts with `env_clear()` plus an allowlist (`exec::ENV_ALLOWLIST`), tested. The server no
  longer holds tokens: `make dev` passes only `KODEHUB_ENV_FILE` (a path). `claude-as.sh` sources
  the file in its own process, and `claude-as.sh --list-names` feeds the selector with names
  only.
- **Sandboxing (critical).** Builds and tests now also run under `sandbox-exec`: no network, and
  no reads of `~/.ssh`, `~/.claude*`, `~/.aws`, `~/.config/anthropic`, `~/.netrc` or the env
  file's directory (tested). Dependencies are fetched first (`cargo fetch`, which runs no
  third-party code), then compiled `--offline` inside the sandbox.
- **Browser origins (critical).** Permissive CORS is replaced by an origin allowlist (localhost,
  127.0.0.1, Tauri). Middleware rejects any request, WebSocket upgrades included, whose `Origin`
  is not allowed. Requests without an Origin (CLI, MCP, agents) still work.
- **Keys and collections validated before reaching REST URLs (high).** Collections must be
  `[a-z0-9_]`, keys use ArangoDB's character set minus `/`, `%`, `?` and `#`, and `..` is
  refused. The generic edge API only accepts real, non-protected edge collections.
- **Test results can't be forged by test output (high).**
  - Rust: only result lines before the failure section count, every name at most once, and the
    totals must match libtest's summary.
  - Python: results come from pytest's JUnit XML.
- **Closures include every referenced symbol (high).** Seed import unions body-inferred
  references into the declared edges. Inference ignores `.method(` calls on foreign types.
- **Merge integrity (high).** Proposals record `base_version`. Merge fails if the symbol moved
  since, and explicit code edges between untouched symbols are applied. Reviews carry a
  fingerprint of the proposals they saw, so `review_ok` requires the last review's fingerprint to
  equal the current one (invariant 6).
- **Medium items:**
  - Visibility is part of the closure hash.
  - The cache path includes a renderer + toolchain tag (`rustc -V` / `python --version`).
  - Only deterministic compile failures are cached.
  - Each closure gets its own harness binary name (`lr_<closure16>`).
  - Crate specs are restricted to registry name/version/features, and pip specs to plain
    requirement specifiers.
  - A changeset-scoped MCP session forces its changeset.
  - `/` is percent-encoded in tool paths.
  - Pin conflicts no longer block every write.
  - ArangoDB binds to 127.0.0.1 only.
  - The test timeout is 300 s.
  - An invalid config file logs a warning.
  - The token-name pattern is no longer editable over the API.
  - Python run now converts dicts to class instances for class-annotated parameters.
  - Integration tests build the binaries first.
- **Not done (pilot):**
  - Exact pinning / lockfiles for third-party crates and packages. Caret requirements remain, so a
    new upstream release can change an artifact without changing its key.
  - Per-test isolated renders.
  - Transactional read-modify-write for changeset documents. The engine is the only writer per
    changeset and is serialised by its running-set guard.
  - Deleting edges removed from seed YAML.
  - Distinguishing duplicate methods across classes.


## D-025 Agents return structured JSON; the workflow engine applies it (M5)
- **Context:** Doc 04 says agents act through the graph API (MCP tools). Letting agents call write
  tools freely makes the changeset state machine hard to control and makes recorded mode replay
  tool side effects.
- **Decision:** Agents get **read** tools over MCP (get_symbol, traverse, coverage, impact, …) and
  finish with one JSON object (plan / proposals / findings). The workflow engine applies that
  result through the same graph API (`propose_version`, `submit_review`, …).
- **Consequence:** Every write is still a graph API call and traceable. The recorded backend
  replays only the final JSON. Blind review is easy to enforce because the engine builds every
  prompt.

## D-024 Merge orphan check runs inside the merge transaction
- **Context:** "No orphans introduced" depends on the graph *after* the merge.
- **Decision:** `merge_changeset` applies everything in one ArangoDB stream transaction, runs the
  orphans AQL inside it, and aborts if new orphans appear. `GET /changesets/{k}/checks` runs the
  same code with `dry_run` (always aborts).
- **Consequence:** There is one implementation of the orphan query (AQL) and no in-memory copy
  that could drift from it.

## D-023 Proposed versions are real version nodes; new symbols wait until merge
- **Context:** Invariant 1 says every symbol has exactly one `current_version`. A symbol proposed in
  a changeset has none yet.
- **Decision:** Proposed versions are inserted immediately (they are immutable and
  content-addressed, so this is harmless) and linked by `proposes`. *New* symbols live only in the
  changeset document until merge. The calls and depends_on edges of proposed code are inferred
  from the body (`kodehub_core::infer`), so edges can't drift from code.
- **Consequence:** An overlay graph (current graph + proposals) is built in memory for tests and
  ghost nodes. The invariant holds at all times.

## D-022 Node.js is a build-time tool only
- **Context:** The user asked "are you using Node? I thought Rust?".
- **Decision:** Server, CLI, store, renderer, runner and agents are Rust. The spec's desktop stack
  (Tauri v2 + React/TS + Vite, Cytoscape.js, xterm.js) needs Node to bundle the UI and to run
  Playwright. At runtime there is only the Tauri (Rust) shell plus static files.
- **Consequence:** `brew install node` was needed on the dev machine. A pure-Rust UI (Leptos) was
  considered and rejected: it leaves the spec and still needs the JS graph and terminal libraries.

## D-021 Headless-agent token selector (exception to "never forward credentials")
- **Context:** The developer keeps several long-lived Claude Code tokens in env vars
  (`DEV0_TOKEN` … `DEV9_TOKEN`), sourced from a file outside the repo. They want to choose which
  one starts new agent work.
- **Decision:** Kodehub stores and returns only the **name** of the variable
  (`agents.token_env`, validated against `agents.token_env_pattern`, default
  `^DEV[0-9]+_TOKEN$`). Agents start through `scripts/claude-as.sh <NAME> …`, the only code that
  dereferences the value. It exports `CLAUDE_CODE_OAUTH_TOKEN` for that one child process and
  `unset`s `ANTHROPIC_API_KEY`. The server lists only the names of matching variables that are
  set. `ANTHROPIC_API_KEY` is rejected as a token name. Set `KODEHUB_ENV_FILE` in a git-ignored
  `local.mk` so `make dev` sources the file in the shell before starting the server.
- **Consequence:** No token value is ever written to disk, logged or sent to the Messages API. The
  UI selector sits in the status bar.

## D-020 Claude Code CLI flags (verified against `claude` 2.1.318)
- **Decision:** The headless agent invocation is
  `claude -p <task> --output-format stream-json --verbose --mcp-config <json> --strict-mcp-config
  --tools "" --allowedTools mcp__kodehub__<tool>… --permission-mode dontAsk --setting-sources ""
  --no-session-persistence --append-system-prompt <role prompt> --model <model>`.
  `--tools ""` disables every built-in tool (no file system or shell). `--strict-mcp-config` and
  `--setting-sources ""` keep the user's own MCP servers, hooks and settings out of agent
  sessions.
- **Not used:** `--bare` forces `ANTHROPIC_API_KEY`/apiKeyHelper auth and never reads OAuth, which
  would break subscription use. The CLI has no `--max-turns` flag, so the engine enforces a
  wall-clock timeout instead.
- **Consequence:** The live M4 test (`m4_claude_mcp.rs`) proves Claude Code lists requirements and
  runs coverage over MCP on a subscription token.

## D-019 MCP server is hand-written, stdio, in `kodehub-cli`
- **Decision:** `kodehub mcp [--read-only] [--tools a,b] [--changeset k]` speaks JSON-RPC 2.0
  (newline-delimited) and forwards every tool call to the HTTP API. The tool catalog
  (`kodehub_core::tools`) is the single definition shared by MCP, the API agent backend and docs.
- **Consequence:** HTTP and MCP are identical by construction. No MCP SDK dependency.

## D-018 The CLI talks only to the HTTP API
- **Decision:** Every `kodehub` subcommand is an HTTP call. `kodehub seed <dir>` posts the local
  path and the server reads it (the pilot is local only).
- **Consequence:** "The graph API is the only way anything reads or writes the graph" holds for
  the CLI too. The server must be running for the CLI to work.

## D-017 Seed: `rand_core` with `getrandom` is declared explicitly
- **Context:** `argon2 0.5` doesn't enable `rand_core/getrandom`, so `OsRng` is configured out.
- **Decision:** `hash_password`'s `attributes.crates` adds
  `rand_core = { version = "0.6", features = ["getrandom"] }` and uses `rand_core::OsRng`.

## D-016 Impact includes tests that exercise the symbol
- **Context:** The M3 criterion is "impact(hash_password) returns the group and both requirements".
  Nothing in the R1 satisfier's call tree reaches `hash_password`, but every R1 test calls
  `UserStore::add_user`, which calls `hash_password`.
- **Decision:** Impact traverses inbound `calls`/`depends_on`/`contains`/`member_of`/`binds_to`,
  then collects `satisfies` targets of every ancestor, `verifies` targets of every ancestor that
  is a test, and every group that (transitively) includes those requirements.
- **Consequence:** Requirements are "at risk" whenever one of their tests depends on the change,
  which is the honest answer.

## D-015 Coverage and health vocabulary
- **Decision:** Requirement status is `pass` (every relevant test passes at its current closure
  hash), `fail` (any fails), `at_risk` (any result missing or stale, or the requirement was
  edited and not yet re-merged), or `uncovered` (no tests). Map health adds `orphan` (grey).
  `uncovered` shows amber in the UI. Group status folds members (`all_of` = worst,
  `any_of` = best) with the group's own satisfier tests.

## D-014 Test execution
- **Decision:** Tests of one language run as one batch: one rendered project, one `cargo test` /
  `pytest -rA` run, output parsed per test. If a batch fails to *build*, each test is retried
  alone so one broken closure doesn't fail unrelated tests. Results are stored per
  (test version, test closure hash). A cached **pass** is never re-run, and failures are always
  retried. The `pytest-json-report` plugin isn't used; parsing `-rA` output is enough.

## D-013 Python toolchain
- **Decision:** One shared venv at `$KODEHUB_HOME/pyenv`, created with `uv` (falling back to
  `python3 -m venv`), with `pytest` plus every `attributes.packages` entry installed on demand.
  The renderer emits a `kodehub_gen` package. Tests import it and inject its names. The run-now
  harness is `kodehub_run.py` beside the package.

## D-012 Run now execution details
- **Decision:** The harness prints its result on a line prefixed `\x1eKODEHUB_RESULT ` so the
  function's own stdout is preserved separately. Processes run in their own process group. On
  timeout (10 s) the whole group is `SIGKILL`ed. On macOS the process runs under
  `sandbox-exec "(version 1)(allow default)(deny network*)"`, so there is no network access
  (tested). `ANTHROPIC_API_KEY` is removed from every child environment.

## D-011 Rust build cache layout
- **Decision:** Each closure renders to a crate named `lg_<closure[..16]>` (deterministic, unique
  per closure) inside a temp dir under `$KODEHUB_HOME/render/`. Every build shares
  `CARGO_TARGET_DIR=$KODEHUB_HOME/cargo-target`, so third-party crates compile once. Builds are
  serialised by a lock, and the harness binary is copied to
  `$KODEHUB_HOME/cache/<closure>/<target>/kodehub_run` with an `artifact.json` marker.
  Rendered `lib.rs` and the manifest are kept next to the artifact for inspection. Failed builds
  are cached too: same closure gives the same failure.
- **Consequence:** A run-now cache hit is just an exec of a cached binary (well under 1 s).

## D-010 Rust structs and enums get serde derives automatically
- **Context:** Run now needs to deserialise arguments and serialise results.
- **Decision:** The renderer appends `serde::Serialize, serde::Deserialize` to every struct and
  enum, and every rendered crate depends on `serde` + `serde_json`. Opt out with
  `attributes.serde: false`. Other attributes are `derives`, `item_attrs` (raw attribute lines),
  `uses`, `crates`; for Python, `imports`, `packages`, `decorators`.

## D-009 Methods
- **Decision:** A method is a `function` symbol with a `member_of` edge to a `class` symbol. Its
  display name is `Class::method` (Rust) or `Class.method` (Python), and code uses the last
  segment. Rust methods render inside one `impl Class { … }` block after the struct (only the
  methods in the closure). Python methods are indented into the class body. Run now refuses
  methods and associated functions in the pilot.

## D-008 Signatures are name-free; names are composed at render time
- **Context:** Doc 08 plants "a duplicate of `verify_password` under another display name" that
  the duplicates query (same `content_hash`) must find. If the name were part of the hashed
  signature or body, the two could never share a hash.
- **Decision:** `signature` is stored without the item name (Rust `(password: &str) -> String`,
  Python `(username: str) -> dict`) and `body` without the enclosing braces/`def` line. The
  renderer writes `pub fn <display_name><signature> { <body> }`.
- **Consequence:** Identical logic under two names shares one version node (two
  `current_version` edges from one version, one per symbol). Renames change the rendered code,
  so display names feed the **closure** hash (D-007) but never the content hash.

## D-007 Closure hash = hash of the whole resolved closure
- **Context:** The spec defines closure_hash recursively (content hash + dependencies' closure
  hashes). Mutual recursion, which is legal in Rust and Python, makes that recursion
  ill-defined.
- **Decision:** `closure_hash(S) = BLAKE3("kodehub-closure-v1" ‖ "root:S" ‖ sorted lines
  "id:display_name:content_hash" for every member of S's transitive closure)`, with members
  resolved to current or pinned versions.
- **Consequence:** It changes when anything reachable changes (the property the spec needs). It
  is well-defined with cycles and cheap to recompute. A changed dependency changes exactly its
  dependents' hashes (tested in M1).

## D-006 Closure hashes are stored on symbol documents
- **Decision:** `symbols.closure_hash` is recomputed (in the same transaction) after every version
  write and every code-edge change, from the in-memory `CodeGraph`.
- **Consequence:** Coverage / health are pure AQL joins of `test_runs` on
  (test version, closure hash). Pilot scale makes full recomputation cheap. Incremental
  recomputation is a later optimisation.

## D-005 Enforcing invariants in the database
- **Decision:** Invariant 1 is a unique persistent index on `current_version._to`, and the
  versioning rule always removes the old edge before inserting the new one in one stream
  transaction. Invariant 5 is a unique index on `currently_running._from`. Invariants 2 and 4: the
  store refuses updates to `versions`, `releases`, `test_runs` and `runs`, and the generic edge
  API refuses `current_version`, `previous_version`, `included_in`, `deployed_to`,
  `currently_running`, `previously_ran` and `proposes`. `GET /api/invariants` reports violations.
- **Consequence:** "Attempting to create a second current_version edge fails" is enforced by
  ArangoDB itself, not only by application code.

## D-004 Requirement history
- **Decision:** Every edit snapshots the prior document into `requirement_history` (collection
  added to doc 02's list) and bumps `revision`, all in one transaction. An edit that changes
  nothing is a no-op (no revision bump), which keeps seed re-import idempotent.

## D-003 Seed format
- **Decision:** `examples/auth-poc/<collection>.yaml` (lists of documents with explicit `_key`),
  `symbols.yaml` (symbol + first version inline) and `edges.yaml` (map of edge collection →
  `[from, to, {data}]`). Edge keys are a deterministic hash of (collection, from, to), so
  re-import creates nothing new.

## D-002 ArangoDB access
- **Decision:** A thin `reqwest` wrapper (`kodehub-store::arango`) uses stream transactions
  (`/_api/transaction/begin`) for multi-step writes. Integration tests each create and drop a
  uniquely named database. The default dev database is `kodehub` on `127.0.0.1:8528`
  (password `kodehub`, docker compose).

## D-001 All pilot data lives inside the repo checkout
- **Context:** The user asked that all files be stored in `~/dev/kodehub`.
- **Decision:** `KODEHUB_HOME` (default `~/.kode` per spec) is set by the Makefile to
  `<repo>/.kode` (git-ignored). It holds config, cache, render dirs, the cargo target and the
  Python venv. Tests use `<repo>/.kode/test`.
