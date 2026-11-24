# Kodehub v0.1 Requirements: Layered Spec for Claude Code

## 1. How to use this document

This spec is written for Claude Code to apply on top of the existing Kodehub codebase (roughly v0.05). It is a layering instruction, not a rewrite. Where this document conflicts with earlier decisions or existing behaviour, this document takes precedence. Where it is silent, keep the existing behaviour. Do not rewrite working components just to match the wording here; extend, refactor incrementally, and keep the system running at each step.

One thing must be preserved: the existing web UI's primary graph rendering. The author likes how it looks and draws nodes. All new UI work builds inside and around that graph. According to the public repository README, the existing code is a Rust workspace (core, store, render, runner, agents, server and CLI/MCP crates) with a React and TypeScript UI built on Cytoscape, ArangoDB run through Docker Compose, and agents that drive the Claude Code CLI. Treat that as a hint and confirm it in the audit.

Before changing anything, audit the current code against this document, list what already exists, what is missing, and what conflicts, and then plan the changes in small steps.

## 2. Philosophy

Kodehub asks what software development looks like when AI does 100% of the coding. Many current paradigms exist only because many humans work on many laptops: Git, GitHub, pull requests, local clones, text files, and merge-time conflict resolution. None of these make sense for a fleet of cooperating agents.

Core principles:

- One centralized, live codebase. Agents do not keep private copies that drift and are reconciled later. All changes are submitted to the shared system as they happen.
- Code lives in a graph database, not in text files. Text files are produced only when something needs to compile or run.
- Conflicts are resolved by agents negotiating in real time, never by human arbitration.
- Humans set direction and strategy: the philosophy, the requirements, the high-level architecture, and the context level. Agents do all coding, including detailed technical discussion.
- Kodehub competes with Git and GitHub rather than building on them. A human who wants to write code by hand should use Git and GitHub. Git exists only as an import and export bridge for existing codebases (section 13), and Kodehub never depends on it internally.
- Cost discipline: no LLM call ever happens while a test runs. LLMs create, repair and review work (blind code review, test-quality checks). They never run or grade tests. Running tests is deterministic and cheap, so it can happen as often as needed.

## 3. System architecture

Everything runs on a Kubernetes cluster, and every server-side component is written in Rust. The browser front end of the web UI keeps its current technology, because the author wants to preserve how the existing graph looks; do not rewrite it in Rust. The Tauri desktop shell stays as a wrapper around the same web UI.

Kubernetes includes the development and test framework itself. On a laptop the cluster is Kind. `make check` runs unit tests with cargo, and runs integration tests against a local Kind cluster with the real build, test and agent containers. Docker Compose is kept only until the Kind path is complete, then removed.

### 3.1 Components

- Web UI: the human interface. Talks only to the API.
- API service: the single front door for the UI, external callers and webhooks. It calls the orchestrator. It may live in the same container as the web UI or separately.
- MCP service: a thin wrapper over the API so that external AI tools can use Kodehub. It adds no logic of its own.
- Orchestrator: the brain. It receives requests from the API, owns all data access and all activity, creates and dispatches agents, build containers and test containers, collects their output, and persists everything to ArangoDB. It can push results outward through APIs, webhooks, the UI and MCP.
- Scheduler: a separate, thin container (see 3.3).
- ArangoDB: the single source of truth, used as a graph database. It stores code, versions, requirements, users, organizations, projects, work items, schedules, test runs and activity.
- Agent containers, build containers and test containers (see sections 4 and 5).
- Object storage tier: holds large binary artifacts and serves as backup for high availability and disaster recovery. Arango holds the metadata and hashes; the bytes live here.

### 3.2 Orchestrator scaling

The orchestrator is stateless and horizontally scalable. Because ArangoDB is the source of truth, two, three or more orchestrator replicas can run at once, provided every state change is written to Arango immediately and no replica depends on local memory for correctness.

### 3.3 Scheduler

The scheduler is its own container, kept very light, so it does not get bogged down when the orchestrator is busy. It only finds due work and hands it off. One or two replicas should run.

Claim protocol, used by the scheduler, the work queue and any other competing consumers:

- A schedule or work record is claimed with an atomic, transactional update in Arango that sets the owner id and a lease expiry.
- If the update succeeds, the claimant runs the item. If another replica has already claimed it, the claimant skips it.
- If a worker dies, the lease expires and the item becomes claimable again.
- This protocol is deliberately the same everywhere, so the scheduler could later be folded into the orchestrator without changing the data model.

The scheduler fires on timers and on events. Timer examples: run the full project test every 24 hours. Event examples: a new code version has landed, so test it now. Humans and agents can also trigger a run through the API, for example a Test now button in the UI.

### 3.4 Storage service

A storage service container sits in front of file and artifact storage and exposes one S3-compatible interface, so the rest of the system has a single code path. It supports three modes, chosen per installation:

- Local only: everything is stored on the cluster's own machines, for example with MinIO. The user accepts the risk of losing it.
- Cloud only: the service writes straight through to S3 or a cloud equivalent, accepting the network and storage cost.
- Hybrid: store locally for speed, and on a schedule of every X hours send backups to a cold tier such as S3 Glacier, so the loss of the whole cluster does not lose the code.

Backups are bundled per project, as a first full snapshot followed by incremental ones, so uploads stay small and a single project can be restored alone. The ArangoDB database, which holds the graph itself, gets the same off-site backup.

### 3.5 Request flow

A user acts in the web UI. The UI calls the API, which calls the orchestrator. The orchestrator writes to Arango, creates or notifies agents and containers, takes their output, saves it back to Arango, and projects results to the UI, MCP or webhooks as needed.

## 4. Agents and the work queue

### 4.1 Agent model

There is one very small base container image called Agent, like a base class. At spawn time the orchestrator instantiates it as a specific agent type by supplying a role, instructions, tools and context. Keeping the image tiny matters, because many agents may run at once.

Built-in types to start with (the design agent in 15.4 and the sysadmin agent in 11.4 are also built in):

- Coding agent: changes code at the code node level.
- Test author agent: writes deterministic shell tests and checks their quality, for example catching a test that a hard-coded value would satisfy. It maintains the project's test container definitions (5.4.1). It does not run formal tests.
- Review agent: blind code review of a changeset (5.8). It sees the requirements, the philosophy document, the diff and the tests, never the coding agent's reasoning or conversation.
- Planning agent: breaks direction and requirements into work items.
- Architect agent: makes higher-level decisions only. It resolves hard failures that survived the retry limit, requirement and use-case conflicts, and negotiation deadlocks, and it proposes new containers, components or contexts. It does not do routine review.
- Import agent: maps an imported Git repository onto the C4 graph (13.1).

Users must later be able to define custom agent types, for example a policy agent that checks regulatory rules, using the same base image and a role definition stored in the graph.

### 4.2 Agents are ephemeral and stateless

This is a rule, not a preference. An agent claims a work item, loads context from the graph, does the work, runs the relevant tests for the code it touched, writes its change log and results through the orchestrator, and then tears down. It never idles waiting for build or test results, so a concurrency cap of 10 coding agents means 10 agents actually working.

Crash safety follows: any agent can die at any moment and nothing is lost. A later optimization, not required now, may let a coding agent hold its slot for a minute or two when a build and test are expected to be very fast. The stateless path must remain the fallback.

Change logs are saved to the graph for every piece of work, so future agents and humans can see what was changed and why.

### 4.3 Concurrency caps

Caps per agent type are layered settings (section 11), tuned by the sysadmin agent to fit the cluster and overridable per project, for example no more than 10 coding agents and 2 test author agents. The orchestrator enforces them when it claims work from the queue and spawns agents. All agents are spawned from the same place, so the caps are honoured.

### 4.4 Work queue

A prioritized work queue is required, because even AI agents have finite capacity. It lives in ArangoDB and uses the same claim-and-lease protocol as the scheduler, so it scales with the number of orchestrators and needs no new infrastructure.

Each work item records its type, target node, priority, original task, change log history, linked failing tests, retry count, and claim owner and lease expiry. Failures and regressions enter the queue at high priority.

### 4.5 Escalation

Humans are not the escalation path for code problems. A human at the strategy level has no useful context on low-level failures.

1. A work item that fails to fix its problem is retried as a fresh work item carrying the full history.
2. After about three rework cycles, the item is handed to an architect or fixer agent. The retry limit is configurable.
3. The architect agent may create new containers, components or code freely, subject to a project setting for how engaged the human wants to be.
4. New contexts, which are the largest areas of the application, are held for human approval by default.
5. A human is contacted only for true external blockers that no agent can resolve: a revoked API key, missing permissions, or a retired external service. The architect agent raises these, clearly marked as unrelated to the code itself.

## 5. Code model, builds and testing

### 5.1 C4 graph model

Code is stored in ArangoDB as a labeled property graph following the C4 model: context, container, component, code, and optionally individual functions or classes beneath the code level. Every level is a node with parent and child edges. Humans normally work at the context and container levels; agents own everything below, and a project setting can let humans go lower.

Level definitions:

- Context: the highest level of architecture, answering "what does this whole area of the system do?" Examples: Web System, Data Systems, Financial Systems.
- Container: often a deployable unit such as a Docker container, but it can be any meaningful collection of components. Example: a Kong API gateway container inside the Website context.
- Component: a collection of code serving one small, common purpose, such as one service behind that gateway.
- Code: a class, module or function. It is not a file. A code node often lines up with a file in the rendered output, but Kodehub deliberately breaks the link between files and code.

### 5.2 Code nodes and versions

A code node holds metadata only. Edges lead from it to its code version nodes (`codeVersion`). A version node holds the content itself, plus its deterministic `content_hash`, and is immutable until it is eventually aged out. Each edge from a node to a version carries `validFrom` and `validTo` timestamps that never overlap across that node's versions, as in a type 2 slowly changing dimension. History is preserved without changing or relinking the code node.

The same pattern applies at every C4 level. Contexts, containers and components also have immutable version nodes (`contextVersion`, `containerVersion`, `componentVersion`) holding their definitions, such as description, interface and container spec, joined by edges with the same validity timestamps. Releases (section 16) point at versions on all levels.

- Every change creates a new version node with the next version number for that node (`getUUID().v124` after `v123`). Versions are never edited.
- Version states: a candidate version is in a changeset and has not yet passed its gates. Its edge has no `validFrom`. A stable version passed its gates: its edge has `validFrom` set and `validTo` empty. A retired version has been superseded by a newer stable version: its edge has a `validTo`. The system must clearly mark every version as candidate, stable or retired.
- A node has at most one stable version at a time, and none while it is only planned. Because the intervals never overlap, the stable version at any past moment can be looked up, which is what lets a release select "all code as of 10/31/2025" (16.2).
- Promotion closes the old stable edge (`validTo` = now) and opens the candidate's (`validFrom` = now) in one transaction, as part of its changeset (5.8), after the gates in 5.6 pass.
- A version's state on its node is separate from release membership. Which versions a release uses is recorded only by that release's `supports` edges (section 16), so one version can belong to many releases.

Build artifacts are not stored on the version node. Building the same version against a changed dependency produces a different binary, and builds happen after the version exists, so writing artifact data onto the version would break immutability. An artifact node is keyed by the `closure_hash`: the version's `content_hash` plus the closure hashes of everything it depends on. It links to the version by an edge and records the output hash and storage location.

### 5.3 Build containers

Building is separate from testing. A build container is deterministic and not an agent, with no LLM access. When an agent submits a change, the orchestrator sends the new version to a build container, which compiles it and publishes the artifact keyed by a hash of the source and its dependencies (the `closure_hash`). The orchestrator writes an artifact node holding the hash and location and links it to the version (5.2). The bytes go to the object storage tier.

Test containers fetch the artifact for a matching hash rather than compiling. Compiled languages such as Rust therefore test from binaries, and interpreted languages such as Python download the source. A build is done once and tested many times.

### 5.4 Tests are deterministic shell

Tests are plain Linux shell scripts. No LLM call happens when a test runs, which keeps large test suites affordable. LLMs appear only to write a test, through the test author agent, or to repair failing code.

Test containers are ephemeral and are not agents. Given a target, such as a code node, a component, a container or the whole project, a test container fetches the artifacts and test scripts it needs from the graph and storage, runs them, writes structured results back as test run nodes, and exits. Each test run node records the exact code version and hashes it ran against, so any count such as 11 of 12 is traceable.

A shell test is the runner contract and the unit of a test run. The script may call rendered language-level tests (cargo test, pytest), a generated CLI harness, or a headless browser. Existing language-level tests are wrapped by shell scripts, not rewritten.

#### 5.4.1 Test container types

Each project owns a set of test container types, stored in the graph as children of the project (`testContainers` → `testContainer`). Each test declares which test container type it needs. Each type declares what it requires, for example Rust, Python, TypeScript or Node, or a headless browser, and the test author agent keeps that set viable.

To keep images small, all types are built as layers on one minimal core test image: a core layer, a language toolchain layer per language, and a browser layer. A type is a combination of layers, and each layer is built once and shared. Front-end use case tests (15.1) use a type that includes the browser layer, so the weight of the browser only lands on the tests that need it. Test container images are built by build containers and stored like any other artifact.

### 5.5 Agent pre-submission tests

Before handing off, a coding agent must run the tests for the code it touched, using the same shell scripts the formal run will use. It records the results as pre-submission test runs on the candidate version, labelled informal. If they fail, the agent fixes the problem before submitting.

The formal run is authoritative. It happens in a clean container against the exact built artifact by hash, and only formal runs count toward promotion. Agents do not grade their own work.

### 5.6 Handoff, promotion and rework

1. A coding agent finishes, records its change log and pre-submission tests on its changeset, hands the work to the orchestrator, and tears down.
2. The orchestrator requests builds for every candidate in the changeset and attaches the artifacts.
3. In parallel, the orchestrator spawns a review agent for a blind review, if the project's `blind_review` setting is on (it is on by default). The review agent records findings on the changeset and tears down.
4. The orchestrator starts deterministic test containers. Each accepts the artifacts, or the code location for interpreted languages, runs the prescribed shell tests, collects the results and reports them to the orchestrator, with no LLM call. Formal tests run at the code level, then up through the parent component, so a change is validated alongside its peers. How many levels up is a project setting.
5. If the gates pass (no new failing tests, no orphan code introduced, no unresolved blocking review findings when review is on), every candidate in the changeset is promoted to live in one transaction.
6. If new failures appear that were not failing at the last known baseline, the orchestrator queues a high-priority rework item, and a fresh coding agent fixes either the code or the test. If the same tests were already failing before the change, the candidate is not blamed. A blocking review finding also creates a rework item. If the coding agent disagrees with it, the two exchange up to the negotiation round limit (10.3) through the changeset, and an architect agent decides on deadlock.

### 5.7 Tiered test cadence

The scheduler runs tests on a per-project policy with sensible defaults, so a small change does not retest the entire project. The defaults are in 11.3 and can be changed per project, and the daily full-project run is always kept. Each tier retests only what changed beneath it. Anyone can also trigger a run on demand through the API and the UI. Failures at any tier become high-priority work items.

### 5.8 Changesets

A single change often touches several nodes at once. For example, a signature change touches a function and every caller. If those candidates were promoted one at a time, a caller could break while its own candidate is still waiting. A changeset groups them so they move together.

- A changeset may name the open releases (16.2) its new versions should join. When it is promoted, its versions become stable on their nodes, and each named release's `supports` edge moves from the old version to the new one. Releases not named can pick the new versions up later through their selection rule.
- Every work item that changes code or definitions owns exactly one changeset. An absorbed work item (10.2) folds into the survivor's changeset.
- A changeset holds the candidate versions it proposes, at any C4 level, along with the change log, the pre-submission test runs, the review findings and the formal test runs.
- The candidates are built and tested together. Formal tests resolve every dependency to the changeset's own candidates first and to live versions otherwise.
- Promotion is all or nothing: every candidate's edge opens and every replaced live edge closes in one transaction.
- States: open → submitted → building → reviewing and testing (in parallel) → ready → promoted, with rework looping back to open, and rejected as a terminal state. When review is off, the reviewing state is skipped.
- A changeset is not a branch. It never holds a private copy of the codebase. Its candidates sit in the shared graph next to live versions, visible to every agent, and they are resolved against the current live graph when built.

## 6. Users, organizations and projects

The existing system has a single implicit project. That must change: a project becomes the owner of all code, requirements, tests, work items and schedules.

- Users: multiple users can sign in. Authentication and authorization are required, and ownership and permissions are modeled as edges in the graph.
- Organizations: users contribute to organizations (see section 11). A project can be private to one user, shared with an organization, or shared with specific named users.
- Projects: every graph node of code, requirements, versions, test runs and work items belongs to exactly one project. Sharing is an edge from a user or organization to a project, with a permission level.
- All API, MCP and UI calls are scoped to the caller's permissions. Agents act within the permissions of the project they were spawned for.

### 6.1 New project flow

The project page is likely the first screen after sign-in. Creating a project asks for:

1. Name and description.
2. Sharing: private, an organization, or named users.
3. The philosophy document, prompted at creation.
4. Starting contexts, chosen with checkboxes from a list of common contexts such as data platform and authentication and authorization.
5. A template: the user's own saved template, or a built-in master template that follows best practice, or start from scratch.

Creating the project builds the context layer from those choices. Over time the system should offer reusable templates, so an organization with many projects does not reinvent common contexts such as authentication. Templates can also be copied from another project the user can access. Users may create bespoke contexts for their domain, for example capital markets, and place them in the graph.

## 7. Requirements

Requirements are the guardrails that keep agents on target. Some form of requirements may exist in the current code; extend it to this model.

### 7.1 Requirement kinds and the philosophy document

- Business requirements: what the business needs.
- Technical requirements: how the system must be built or must behave.
- Philosophy document: one per project. It states the high-level strategic goals and the vision, and is not a requirement itself. It is the tiebreaker when requirements are ambiguous, missing or in conflict.

Decision: business and technical requirements share one requirement node type with a kind field, so the kind list can grow later beyond those two. All requirements are always gathered and evaluated together by inheritance. The philosophy document is a separate, singular node type attached to the project, because its role as tiebreaker differs from a requirement. Both must be queryable by kind and by level.

### 7.2 Levels and inheritance

Business and technical requirements attach at any C4 level: project, context, container, component, code. Each level inherits everything above it. A coding agent working on a code node traverses up to the project and merges all inherited requirements with its own into one master list that guides the work.

Best practice, which the product should encourage, is to put each requirement at the lowest level where it applies, since every inherited requirement burdens every agent working beneath it. Code-level requirements will be rare, typically a recorded exception for one edge case.

### 7.3 Conflict resolution

If a coding agent finds requirements that conflict, overlap in mutually exclusive ways, or do not cover an edge case, it hands the question to an architect agent. The architect agent uses the philosophy document to decide. It may propose a requirement change, stating which requirement applies in which situation and why, and send that proposal to the human for approval. Requirement changes are one of the few things the human approves.

### 7.4 Push-down suggestions

The UI should detect a requirement set at a high level that appears to apply to only one context or container, and offer to push it down to that level.

## 8. Web UI

### 8.1 Keep the current graph

The existing primary graph view, including how it draws nodes, stays. Extend it; do not replace it.

### 8.2 Zoomable, collapsing graph

The main code view is a graph of nodes that behave like boxes. Zooming in expands a box and reveals its children; zooming further reveals their children. Zooming out collapses them again. At any zoom level the user can see the level of detail being shown. Users can place nodes where they want, and there may be buttons for automatic layout.

### 8.3 Information lives in the nodes

Most information appears as small, tight elements inside each node, not in sidebars. Do not open side panels in any direction for ordinary detail. Clicking a small element opens an overlay or popup next to it, which can be closed again.

- Test badge: shows tests passed out of total for the last run, for example 11/12, colored by state such as yellow when something fails. Clicking opens an overlay with test runs, history and a Test now button.
- Requirement counts: two short numbers with very small shorthand labels, one for business and one for technical requirements. Each shows the count on that object, and the count for that object plus all its children, so a component might show 10 of its own and 18 with its code children. Clicking opens the requirements for that object, including inherited ones, with a view of the cascade.
- Lifecycle badge: a small colored indicator of the node's lifecycle state (planned, in build, ready for test, live), with parents showing a rolled-up state. Test badges also show rolled-up counts including all children. Also an in-progress indicator showing how many work items or agents are touching the node, a candidate marker, and a last-build status dot.

The goal is that someone traversing the graph gets a lot of information at a glance, with detail one click away.

The existing left and right sidebars and the node panel are retired. Their content (requirements and test status, node detail, agent activity for a node) moves into floating overlays opened from the small badges on each node. For example, clicking a node's orange 11/12 test bubble opens its test overlay. Project-wide views that belong to no single node, such as the work queue, agent activity and settings, become their own screens (8.4), not sidebars.

### 8.4 Screens to add

Project list and create-project flow with speech transcription for the philosophy document, sharing settings, philosophy and requirements editing with the cascade view, a use case view where use cases and their expected-outcome tests are created and moved from draft to live, work queue and agent activity views, schedule settings for the test cadence, and settings for agent caps and how engaged the human wants to be.

## 9. Rollout instructions for Claude Code

This is a layering plan, not a rewrite.

1. Audit the existing code and report what exists for each section of this spec, what is missing and what conflicts. Keep the working system running. Write the audit and the plan into the repository before changing anything.
2. Add the data model first: users (human and agent), organizations, projects, the layered settings hierarchy, requirement nodes, the philosophy node and use case nodes, node and version nodes at every C4 level with `validFrom` and `validTo` edges, artifacts, changesets, test runs, test container types, work items, intents, schedules, releases and environments, tokens and the audit log, all in ArangoDB. Do not migrate v0.05 data. Start a fresh database and re-create the seed project in the new model, owned by a default user. Add local accounts, session tokens and personal access tokens, so that every API and MCP call is authenticated from this step on.
3. Add Kubernetes deployment manifests, with Kind for laptop use, and move `make check` integration tests onto Kind. Then add the storage service container, then the Git import container and import agent, so the author can bring existing GitHub repositories into the graph early. Then add the claim-and-lease protocol, the work queue and the scheduler container.
4. Add build containers and layered test container types, then the deterministic shell test flow, artifact hashing, changeset promotion, and the blind review gate.
5. Reduce agents to the one tiny base image with ephemeral, stateless behavior and per-type concurrency caps, add declared intents and the overlap and negotiation flow, and add the model credential and budget handling.
6. Add the UI: authentication, project flow, in-node badges and overlays that replace the sidebars, the requirements cascade, and releases and environments. Preserve the current graph rendering.
7. Add escalation to architect agents and the philosophy-document conflict flow. Then add the node lifecycle states, the project kickoff chain with planning agents, and tests-before-code, before turning on the test scheduler, so that incomplete nodes are never tested. Then add use cases with their draft and live states and the design agent. Finally add the sysadmin agent, the test author agent's schedule management, the Git export container with off-site backup of storage and the database, and the OAuth flow for remote MCP.

If an earlier decision conflicts with this document, this document wins. If this document is silent, keep what exists. Where something here is ambiguous, choose the simplest approach that fits the principles in section 2, and record the decision in the repository so the author can review it.

## 10. Agent coordination and overlap

Agents never work in isolation and never reconcile after the fact. When two agents would change the same code, they coordinate up front, through records in the graph rather than direct messaging, which fits stateless, ephemeral agents.

### 10.1 Declared intents

When an agent claims a work item, it registers an intent in the graph: the nodes it plans to change and a short description of how. The orchestrator compares each new intent with the active ones. If two intents overlap, the orchestrator opens a negotiation record that links the two work items.

### 10.2 Overlap decision

The orchestrator measures overlap from the registered intents as the Jaccard index: shared nodes divided by all nodes either work item touches. Two items changing 20 nodes each, with 18 in common, score 18 / 22 = 0.82. A project setting, `overlap_absorb_threshold` (default 0.6), defines the score above which absorption is the default.

- Small overlap: sequence the work, so one goes first and the other is re-queued rather than left idle. Alternatively split the shared node into smaller nodes so both can run in parallel.
- Large overlap: absorb. One agent takes on both work items, and the other item is folded into the survivor's task and completed together. For example, two agents each changing 20 nodes with 18 in common should become one agent doing both.
- Shared end state: where parallel work on the same node is truly needed, the agents agree on the end state for the overlapping piece through the negotiation record, and each does its part toward it. This is agreement before the work, not merging after it.

### 10.3 Deadlock and combined tasks

Negotiation is bounded by a set number of rounds, the project setting `negotiation_rounds` (default 3). If agents cannot agree, an architect agent decides using the merged requirements and the philosophy document. No human is involved.

An absorbed work item counts as part of the combined task for retry limits, queue priority and concurrency caps, never as two separate items.

## 11. Users, organizations and layered settings

### 11.1 Users and organizations

- A user is either a human user or an agent user. An agent user is an external agent that drives feedback into the system. It is not one of the containerized agents in section 4. The two types exist so that different defaults can be set for each, and so that audit logs can tell a human's actions from those of that human's agent. Each agent user is linked to the human or organization it acts for; authentication and audit are covered in section 13A.
- A user can contribute to one or more organizations, such as an employer, an open source group, or a group of friends. The relationship between users and organizations is many to many.
- Every organization must have at least one admin, who can add other users. An organization with zero admins can no longer be administered and is retired or archived automatically.
- Every project belongs to either a user or an organization.

### 11.2 Layered settings with inheritance

The system has one set of settings that can be applied at several levels. Each level overrides the one above it, so the lowest level wins. The hierarchy runs: application default, then user, then organization, then project, and where useful lower levels of the C4 graph. A project owned directly by a user skips the organization level, so that user's settings are its defaults. An organization-owned project inherits the organization's settings. Where the organization leaves a setting undefined, it falls through to the application default, not to the contributing users, because thousands of users may hold different values and no single one should win. The exception is per-actor settings, such as model and Git credentials, which always come from the acting user. Organizations should still define the settings they care about explicitly. Project settings, set by the organization's admins, override the organization's. A project inherits an organization's setting unless it overrides it. For example, an organization may allow three failed attempts before escalation while one regulated project allows only one, or sets up a regulatory agent with a very low tolerance for failure.

This needs role-based access control: who may view and change settings at each level is governed by roles on the user, organization and project edges in the graph.

Settings covered by this mechanism include the rework retry limit, test cadence, test budget, agent concurrency caps and how engaged the human wants to be. Retry limit is a per-project setting inheriting from its organization.

### 11.3 Application defaults

If nobody sets anything anywhere, the defaults are deliberately conservative. Retry limit: three rework cycles before escalation to an architect agent. Test cadence by level: code tests immediately when a version lands, components every 2 hours, containers every 4 hours, contexts every 12 hours, and the whole project every 24 hours. A full-project run tests every component, container, context and code node, so it counts as a test of everything beneath it. The daily full-project run is the one cadence that is always enforced.

### 11.4 Sysadmin agent

A sysadmin agent, also called the cluster manager agent, owns the compute side. It wakes on an interval such as every 10 minutes, checks health and utilization, and goes back to sleep if all is well. It sets agent concurrency caps to match the available compute. It forms a hypothesis such as running Y agents on this cluster, applies it, watches for CPU or I/O bottlenecks, and scales back if the cluster is overloaded. It can also move work toward machines with spare capacity. This lets a laptop running Docker and a small Kind cluster support two or three coding agents, while an enterprise cluster supports dozens, with no human tuning. Humans may still set hard limits.

### 11.5 Test scheduling and budget

The test author agent writes and repairs tests, and it also owns the test schedule. It does not run tests itself. Responsibilities:

- Spread tests evenly across time rather than starting every component at the same hour mark, which would spike the system.
- Base each next run on the last time that item ran, so the schedule balances itself once started.
- Recompute the spacing whenever tests are added, for example going from 48 tests per day to 60 and moving the interval from every 30 minutes to every 20.
- Respect a maximum test concurrency, set either as a number of tests or, preferably, as a percentage of cluster capacity, since tests differ in CPU and I/O cost.
- Work with the sysadmin agent to turn a budget, such as no more than 30% of the system for tests, into a concrete plan, for example components four times a day, containers twice a day and the full project once a day.

The scheduler container only fires what the test author agent has planned. The daily full-project run is always kept. It sits outside the test budget, so if a budget is too small to cover it, the daily run still happens and the other tiers are thinned instead.

## 12. LLM access, credentials and budgets

### 12.1 Credentials

Credentials for model providers are layered settings, like all others in section 11, and can be set at user, organization or project level. Authentication is pluggable. For Claude, supported credential types are API keys and long-lived subscription tokens. Nothing is hard-coded to prevent either type.

The UI must make clear that subscription credentials are intended for individual users working on personal projects, such as at home, and are not for production or for organizations, which should use API keys. The author's understanding is that using a subscription token to drive a separate coding harness is permitted. This is not verified, so the author should confirm the provider's current terms of service before relying on it. The product should not depend on that answer, because credentials are pluggable.

Credentials are stored encrypted, never written to logs or to the graph in plain text, and passed to agent containers only for the duration of their work.

Subscription tokens are restricted in code, not only by a warning:

- A subscription token can be set only at the user level, and is used only for projects that user owns directly. Organizations and organization-owned projects accept API keys only.
- A subscription token is only ever handed to the official `claude` CLI inside an agent container. It is never sent to the Messages API. This is a hard rule.
- The environment variable `ANTHROPIC_API_KEY` is never read or set by Kodehub. API keys reach agent containers under Kodehub's own variable name.

### 12.2 Metering and budgets

The orchestrator tracks token usage and estimated cost per project. A budget is a layered setting. When a project reaches its budget, it stops gracefully: the orchestrator stops claiming new work items for that project, lets running agents finish their current item, and flags the project and its admins. Agents are never cut off mid-task.

## 13. Git import and Git export

Kodehub competes with Git and GitHub, but GitHub holds most of the world's code and many people treat code that is not on GitHub as nonexistent. A bridge in both directions is therefore a high-priority feature, not an afterthought, even though Kodehub never depends on Git internally.

### 13.1 Git import container

The orchestrator spins up a Git import container on request. Mapping arbitrary code onto contexts, containers, components and code is a judgement call, so the import runs in two stages:

1. The Git import container, which is deterministic and has no LLM, clones the repository and parses it into candidate code units (functions, classes and modules, not files), using language parsers for Rust and Python. It stores the raw units and their call and dependency edges as a staging set in the graph, then exits.
2. An import agent, a built-in agent type, evaluates the staging set: what the system does, which contexts it spans, which deployable units are containers, and which groups of code form components. It proposes the C4 mapping as a changeset. Once that changeset passes its gates, the code becomes live versions in the project.

Languages other than Rust and Python are imported as opaque modules, one code node per file, marked as not yet decomposed. This is a one-time import into a new project and is needed first, because the author's existing code is all on GitHub. Importing the same repository a second time is effectively a reverse sync that would overwrite existing objects, so it is out of scope for now and deferred until it becomes a real problem.

### 13.2 Git export container

A separate Git export container exports a project's live versions as ordinary files and pushes them to a Git repository such as one on GitHub, on a schedule or on demand. It acts as a backup and a mirror for people who expect to find code on GitHub. It is its own container type and does not ride on the build container, since compiled artifacts and source mirroring are different jobs on different schedules.

### 13.3 Credentials and direction of truth

A Git credential, such as an access token, is a layered setting like model credentials in section 12, set at user, organization or project level, and it is what authorizes both cloning private repositories and pushing. Without it, only public repositories can be imported and Git export is unavailable.

Kodehub is the source of truth and the Git repository is a backup and mirror. Humans who want to edit code by hand belong in Git and GitHub, not in Kodehub. Changes made directly in Git after import are not merged back, and reverse sync is left out for now, to be built later if needed.

## 13A. Authentication and external agent identity

The MCP service has no security of its own. It is a thin wrapper over the API, so every MCP call is authenticated and authorized exactly like a UI call, and is scoped to the permissions of the identity behind it.

### 13A.1 External agents are agent users with their own identity

When a project owner drives Kodehub from an external assistant such as Claude or Claude Code, that assistant acts as its own agent user (see 11.1), never as the human. Each external agent has its own login and credentials, separate from the human who directed it.

Every agent user records the human or organization it acts on behalf of, as an edge in the graph. The two identities are related but distinct: the human sets the intent, and the agent carries it out. This lets auditing separate what a user did from what a user's agent did, and lets different defaults, limits and permissions apply to each.

### 13A.2 How an external agent authenticates

Remote MCP: the standard OAuth flow. The owner connects the assistant to the MCP service and is sent to a Kodehub sign-in and consent page. On approval, the assistant receives a token issued to its agent user, linked to the approving human.

Local or scripted use, such as Claude Code on a laptop: a personal access token issued from the UI to a named agent user. Tokens are stored only as hashes, shown once at creation, and can be revoked at any time.

Both paths are pluggable, so other schemes can be added later without changing the data model. Personal access tokens are built first, together with local accounts and session tokens for humans. The OAuth flow for remote MCP, which makes Kodehub an OAuth authorization server and needs the MCP HTTP transport, follows later.

Containerized agents (section 4) are not agent users. Each one receives a short-lived token from the orchestrator, scoped to its one project and work item, that expires when the work item's lease ends.

### 13A.3 Scope, limits and revocation

- A token is limited to specific projects and a permission level, and never exceeds the permissions of the human it acts for. If that human loses access to a project, the agent loses it too.
- Tokens expire, and expiry and maximum lifetime are layered settings (section 11).
- Revoking an agent user or a token takes effect immediately for every API, MCP and UI call.
- The MCP service accepts only valid tokens. Calls without one are rejected, and no anonymous access exists.

### 13A.4 Audit trail

Every action through the API, including those arriving via MCP, is logged to the graph with the acting identity, the human it acted for if any, the project, the action and the time. The UI must let admins filter activity by human, by external agent and by containerized agent, so the three can be told apart.

Tokens and other credentials never appear in logs or in the graph in plain text, consistent with section 12.1.

## 14. Project kickoff and node lifecycle

A new project starts empty, with contexts, requirements and a philosophy document but no code. The system must actively start building rather than letting tests fail until something appears. Tests that fail only because nothing exists yet would produce a hodgepodge design, and agents might hard-code values just to turn tests green.

### 14.1 Intake

There are two intake paths, and both end in the same graph objects: contexts, business and technical requirements, and the philosophy document.

- In the app: the new project flow includes a speech transcription button for the philosophy document, and similar capture for requirements. The UI records the audio and uploads it to the API. A transcription container running a local Whisper model transcribes it, so the result does not depend on browser speech support, which varies between Chrome and Tauri's WebKit. Saving a raw transcript adds a work item for the architect agent to clean it up into a readable document without losing meaning.
- Through the MCP service: a user can talk through the project with an external assistant such as Claude, which structures what was said and pushes it into Kodehub through the MCP service as requirements, philosophy, descriptions and contexts. This is an explicit, supported path.

### 14.2 Kickoff chain

Creating a project automatically enqueues an ordered chain of work items.

1. The architect agent cleans up the philosophy document.
2. The architect agent evaluates the contexts and requirements for gaps and conflicts, using the philosophy document as tiebreaker, and proposes any additions for human approval where required.
3. Planning agents decompose the project top-down, but only as deep as the component level. A planning agent per context plans its containers, and a planning agent per container plans its components. Each component then gets one coding agent, which is responsible for that component and all the code nodes beneath it. This keeps the agent count in the low hundreds rather than thousands.

Each level of planning is triggered when the level above finishes, so the architecture exists before code is written. Later rework still follows the normal queue rules, where an agent works on a specific code node. Whether components also get a test author agent at creation is to be decided during the first build.

### 14.3 Tests before code

The test author agent writes tests for each node from its merged requirements as part of planning, before coding agents start. This keeps development test-driven and makes hard-coded results less likely to pass. The test author agent checks test quality, for example to catch tests that a hard-coded value would satisfy. The architect agent stays out of routine test review.

### 14.4 Node lifecycle states

Every node in the C4 graph has a lifecycle state, which the UI shows on the node.

- Planned: the node exists but nothing has been built.
- In build: work items are active, or it has been stubbed or is partly built.
- Ready for test: it has a built version and its tests exist.
- Live: the node has a stable version (5.2), which means a version has passed formal tests and been promoted.

The scheduler and test author agent only schedule tests for nodes that are ready for test or live, so incomplete nodes are never tested. When rework starts on a node, it returns to in build. A parent node is not ready for test while any of its children are still planned or in build, unless a project setting says otherwise.

The lifecycle state is shown on every node in the graph as a small colored badge, so the overall graph shows at a glance which contexts and containers are healthy. A parent shows a rolled-up state: it is live only when it and all its children are live. Like the requirement counts, test counts roll up. A node's test badge shows its own tests and also the total including every child, so a context with 20 tests of its own may show thousands in aggregate. Clicking a badge opens an overlay with the detail.

## 15. Use cases

A use case is a node attached to the project that describes something a user of the product being built would do, and what they expect to happen. Examples for Kodehub itself: a user creates a new project, an organization creates a new project, a user starts a test of a container, a user defines a new business requirement. Use cases sit above requirements as the higher-level direction. Instead of hitting business and technical requirements in a vacuum, agents work to satisfy specific use cases.

### 15.1 Model

A use case node has a name, a description, a lifecycle state, and edges to the requirements it relates to and to the tests that define its expected outcome. Those tests are the use case's expected outcome. A use case is a project-level object, and the orchestrator and API must support creating, editing, listing and changing its state like any other object.

Use case tests are the same deterministic shell tests as elsewhere. For front-end behaviour, such as opening a screen and clicking through steps, they drive a headless browser from a shell script, so no LLM is involved at run time.

### 15.2 Lifecycle

- Draft: the use case is being refined and its tests are not scheduled.
- Live: the user is happy with it. Its tests join the test schedule like any others.

A user can move a use case from draft to live in the UI, or an external assistant can do so through the MCP service as its last instruction. When use case tests fail, the failures create high-priority work items, so the application corrects itself until the use case is satisfied. For example, a new use case that says a user sees a list of projects right after logging in triggers a design agent to build that screen and hand it to coding agents.

### 15.3 Creation, conflicts and effects

- Voice intake: a user can talk through a use case with an external assistant connected through the MCP service. The assistant splits it into the use case and its description, and creates the business and technical requirements and the tests it implies, leaving everything in draft until the user turns it on.
- Contradiction check: before a use case goes live, the architect agent checks it against the existing live use cases. If they conflict, the architect agent resolves it using the philosophy document and may propose changes for human approval.
- Effect of new requirements: when a new requirement is added over existing code, the test author agent checks whether existing use case tests still cover it and writes new tests where they do not. Failures flow into the work queue like any others.

### 15.4 Design agent

A design agent is a built-in agent type that handles user interface changes needed to satisfy use cases. It produces designs and hands the work to coding agents.

## 16. Releases and environments

### 16.1 Environments

Each project has an ordered list of environments. The default is DEV → TEST → QA → PRE-PROD → PROD, and the list can be changed per project as a layered setting. In the cluster, each environment of a project is its own Kubernetes namespace. For the pilot, deploying a release means running its containers' artifacts in that namespace.

### 16.2 Releases

- A project has many releases. Each release has its own identifier, such as `r123`.
- A release is the set of version nodes it needs in order to run, at every C4 level (`codeVersion`, `componentVersion`, `containerVersion`, `contextVersion`), each joined to it by a `supports` edge. A version that did not change between releases just gains another `supports` edge, so identical code is never duplicated. Example: code node `getUUID()` has version `getUUID().v123`, which supports releases `r123`, `r234` and `r345`. If release `r456` needs a change to `getUUID()`, the change creates `getUUID().v124`, and `v124` supports `r456`.
- Selection: a new release chooses its versions with a selection rule, recorded on the release. The rules are: newest (the latest version of every node, candidates included, useful in DEV), stable (the current stable version of every node), as of a timestamp (the version that was stable at that moment, read from the validity intervals), or an explicit list. A rule can also filter on tags or any other attribute of the C4 node or its version, for example "stable, but only nodes tagged `payments`" or "newest where `attributes.channel = beta`". Rules are stored as structured data, not code, so new kinds of filter can be added later without changing the data model. Specific nodes can be pinned to a chosen version, overriding the rule.
- Release states: open, then cut. While a release is in DEV it is open. Its selection rule can be re-applied, and changesets can name it (5.8). It is cut, and immutable from then on, when it is first promoted out of DEV.
- Clone: a release can be cloned into a new open release at its own environment or any lower one, never a higher one. With DEV at the bottom and PROD at the top, a QA release can be cloned into QA, TEST or DEV. The clone gets the same `supports` edges plus a `cloned_from` edge to its source. Cloning only copies edges, so it is cheap.
- Fixing a bug found after DEV: a bug found in a release in TEST, QA, PRE-PROD or PROD is fixed by a normal changeset, which creates the fixed version and tests it as usual. The orchestrator then makes a patch release. It clones the affected release at its current environment (for example, TEST's `r456` becomes `r456.1` in TEST), moves the clone's `supports` edge to the fixed version, and runs that environment's gates on the clone. Only the fixed code changes, and nothing has to restart from DEV. The original `r456` is retired and stays unchanged, so cut releases remain immutable and the record of what ran is never rewritten. The same flow is the hotfix path for PROD, where the patch release passes the PROD gate, including human approval, before it replaces `r456`.
- Diff between two releases is the set difference of their supported versions. Bisect across releases finds the change that introduced a failure.

### 16.3 Promotion

- The release, not the code, is what moves through environments. A release is in exactly one environment at a time, recorded as an edge with `validFrom` and `validTo`. Promoting a release closes its edge to the current environment and opens one to the next, in one transaction.
- Each environment has a `max_releases` setting. By default, DEV, TEST and QA hold any number of releases, so several can be at the same stage. PRE-PROD and PROD hold at most one each. Promoting into a single-slot environment retires the release that was there.
- Gates (defaults, configurable per project): DEV → TEST, every container builds. TEST → QA, all formal tests pass, there are no orphans, and live use cases pass. QA → PRE-PROD, a smoke run passes. PRE-PROD → PROD, human approval. Approving production is one of the few things a human approves.
- Rollback means promoting the previously retired release back into the environment, through the same gate.
- Environment gates are deterministic checks run by the orchestrator. They are not LLM agents.

## 17. Open questions

- Resolved: in-node badges are tests, requirement counts, an in-progress indicator showing how many work items or agents are touching the node, plus a candidate-version marker and a last-build status dot.
- Resolved: one requirement node type with an extensible kind field (business, technical, more later), plus a separate singular philosophy node per project.
- Resolved: retry limit, caps, cadence and test budget are layered settings (see section 11), with conservative application defaults and a sysadmin agent tuning compute-dependent values.
- Resolved: S3-compatible storage behind a storage service container with three modes (see 3.4).
- Resolved: pluggable credentials (API keys and subscription tokens) with per-project metering and graceful stop at budget (see section 12). Author to verify provider terms for subscription tokens.
- Resolved: Git import and Git export containers, both high priority, with Kodehub as the source of truth, one-time import only, and reverse sync deferred (see section 13).

- Resolved: changesets group candidate versions so that they build, test and promote together (see 5.8).
- Resolved: blind review is kept as a review agent, gated by the project setting `blind_review`, which is on by default (see 5.6).
- Resolved: overlap uses the Jaccard index with an absorb threshold of 0.6 and 3 negotiation rounds (see 10.2 and 10.3).
- Resolved: releases are sets of `supports` edges to versions at every C4 level. A release picks its versions by a selection rule (newest, stable, as of a time, explicit). It is open in DEV and immutable once cut. Releases can be cloned at their own environment or lower, and bugs found after DEV are fixed through patch-release clones at the same environment. A release is in one environment at a time, and PRE-PROD and PROD hold one release each (see section 16).
- Resolved: there is no migration from v0.05 data. The seed project is re-created in the new model.

Still open, to decide during the first build and record in the repository: whether components also get a test author agent at creation; and, later, re-import and reverse sync from Git.
