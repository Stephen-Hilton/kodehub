# Claude Code prompt: versioned releases model

Apply this change to the Kodehub data model. Audit the current code first, keep the system running at each step, and record any decision you make in the repository.

## Problem

Today each C4 identity node (context, container, component, code) has edges to its own version nodes, but there is no edge between a parent version and its child versions. A release therefore cannot tell which child version belongs with which parent version, except by an edge from the release to every object (thousands of edges per release) or by timestamps (which prevent two releases from being live at once).

## Required model

1. **Two layers.** Identity nodes are stable and version-agnostic. They keep the C4 parent and child structure, and they are the anchor for requirements, locks, agent intents, work items and test history. Each identity node keeps its edges to its version nodes.
2. **Immutable version nodes with structural edges.** Add an edge type from a parent version to its child versions: context version to container versions, container version to component versions, component version to code versions. Each version lists the exact child versions it was built from, all of them and not only the changed ones. These edges are written once when the version is created and never modified.
3. **Changing code.** When a code node changes, create the new code version, then a new version of each ancestor (component, container, context). Each new ancestor version points at the new child version and at the existing, unchanged child versions. Unchanged branches are reused as they are. Cost is proportional to the changed paths only.
4. **Releases.** A release is a node with a single edge to the root version of the project. To read a release, walk down from that root once. Never walk back through earlier releases, and do not add per-object release edges or rely on timestamps to select versions.
5. **Named lines.** Lines such as main or release-2 are movable pointers to a root. Several lines can coexist and share most of their versions. Promoting a candidate means moving a line's pointer. A hotfix means starting a new root from an old root.
6. **Timestamps.** Keep valid-from and valid-to as a convenience index for questions like what was live on a line at a given time. They are not the source of truth for which version belongs to a release. Reconcile the candidate, live and retired states in section 5.2 of the spec with this model, for example live meaning reachable from the head of main, and record the decision.
7. **Merging lines.** Out of scope for now. Choose the simplest rule that fits the existing negotiation and architect-agent mechanisms, and record it.
8. **Concurrency.** Writing a new chain of ancestor versions and moving a line pointer must be one atomic ArangoDB transaction, with a compare-and-set on the pointer so two agents cannot overwrite each other.
9. **Migration.** Build the version-to-version edges for the existing data from the current live versions, create one initial root, and make it release 1 on main.

## Acceptance checks

- Reading release N costs the same regardless of how many releases exist.
- Two lines can point at different roots at once, and unchanged versions have the same ids in both.
- Changing one code node writes one new version per ancestor level plus the edges to their children, and nothing else.
- Requirements, locks and intents still attach to identity nodes and survive across versions.
