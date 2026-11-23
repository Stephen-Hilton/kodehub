# Critical review — Kodehub pilot, milestones M5–M7 and the desktop UI

Reviewer: staff-level critical review (automated agent, claude-opus-5-5). Scope: `CLAUDE.md`,
`docs/00`–`08`, `docs/decisions.md`, `crates/kodehub-agents`, `crates/kodehub-store/src/{changesets,releases}.rs`,
`crates/kodehub-server/src/routes/*`, `app/src`, the screenshots in `.kode/shots/`. Findings
already raised in `review-m4.md` and handled in D-027 are not repeated.
I did not run `make check`. This is a reading review.

## Summary

See the end of the file. It was written last. Short version: the core flows work. Invariant 6
and the green signal can be bypassed (#1–#5). The release gates evaluate the wrong graph or
trust caller strings (#6–#8). UI score: 6.5/10.

---

## Findings

### 1. `at_risk` is a boolean that any merge clears, even an empty one — **high**

- **Location:** `crates/kodehub-store/src/changesets.rs:819-823`; `crates/kodehub-agents/src/engine.rs:215-249`
  (reuses only `draft`/`planned` changesets); `store.rs:768` (sets the flag).
- **Problem:** Merging any changeset that lists a requirement sets `at_risk: false` on that
  requirement. Nothing checks which revision the changeset addressed, whether a newer changeset
  for the same requirement is still open, or whether any test verifies the new edge case.
- **Scenario:**
  - (a) A human adds edge case A (rev 2), and cs1 starts building. They then add edge case B
    (rev 3), which opens cs2 because cs1 is no longer `draft`/`planned`. cs1 merges, R1 goes
    green, and edge case B has no code and no test.
  - (b) The tester and coder return no proposals (a model failure, or "nothing to do"). The
    existing R1 tests pass from cache, the review of an empty diff passes, the merge goes
    through and R1 turns green. The new edge case has no test at all.
  - Both are false greens on the map. That defeats vision point 3 ("validation is a query").
- **Recommendation:**
  - Record `addresses: {req: revision}` on the changeset when it is approved.
  - On merge, clear the flag only if the requirement's revision still equals that value and no
    other open changeset lists the requirement. Otherwise keep it amber.
  - Add a merge check `requirement_covered`. For an edge-case or example edit, at least one
    proposed or modified test must `verify` the requirement, and that test must pass.
  - Add both scenarios to the M5 tests.

### 2. The generic `transition` route can mark a changeset `merged` without merging — **medium**

- **Location:** `crates/kodehub-server/src/routes/changesets.rs:189-205`; `crates/kodehub-core/src/model.rs:261`
  (`(Ready, Merged)` is a legal transition).
- **Problem:** `POST /api/changesets/{k}/transition {"to":"merged"}` goes through
  `Store::transition`. That only patches `state` and `history`, and skips `merge_changeset`, so no
  `current_version` edge moves and no checks run. The changeset then shows as merged, its
  proposals are never applied, and `ensure_editable` locks it for good. `Testing → Ready` can be
  set the same way. `merge_changeset` re-checks this one, but the UI and the engine (state
  `Ready`) trust it.
- **Recommendation:** Accept `merged` only from `merge_changeset`, and refuse it in
  `transition`. Expose the generic transition route only for `rejected`/`escalated` (or drop
  it), since the engine owns the other edges.

### 3. Merge TOCTOU: checks run outside the merge transaction, and concurrent merges aren't serialised — **medium**

- **Location:** `changesets.rs:671` (`merge_checks` reads with no trx), `:678-683`
  (`orphans_before` with no trx), `:685` (trx begins afterwards); `engine.rs:347-352` (the
  running guard is per changeset).
- **Problem:** Two changesets can merge at the same time: one from the engine, one from a
  human clicking Merge, or two engines. Each one computes overlay closure hashes and looks up
  `test_runs` before its transaction starts. The `base_version` check only covers B's own
  touched symbols. If A changes a helper that B's code calls, B's tests ran against the old
  helper and B still merges, which breaks invariant 6 ("fails any affected test"). The orphan
  baseline is also taken outside the trx, so A's orphans can be blamed on B, or B's hidden.
- **Recommendation:** Simplest fix: put a process-wide `tokio::Mutex` in `Store` around
  `merge_changeset` and `deploy`. Better: inside the trx, recompute the overlay from in-trx
  reads, recompute the closure hashes of the affected tests, and require passing `test_runs` at
  those hashes. After A merges, B will (correctly) be blocked. See #9 for how B then recovers.

### 4. Invariant 6's "passing blind review" can be supplied by any API caller — **high**

- **Location:** `routes/changesets.rs:161-178` (`submit_review` takes caller-chosen `by` and
  findings); `changesets.rs:648` (`review_ok` = last review has matching fingerprint and no open
  blocking findings); `kodehub_core::tools` exposes `submit_review`/`merge_changeset` over MCP.
- **Problem:** The fingerprint (D-027) proves the review saw the current proposals. It does not
  prove the review was *blind* or came from the reviewer. An external Claude Code session in
  the embedded terminal, connected through `kodehub mcp` without `--read-only` (doc 05 offers
  this), can call `propose_version`, `submit_review {findings: []}`, `transition` and
  `merge_changeset` on its own changeset. Invariant 6 then holds only on paper. The same session
  can also drop findings: it posts a new review, and only the *last* review counts.
- **Recommendation:**
  - Make reviews server-attested. Only the engine's reviewer path (or a human, through a
    UI-only route) can write a review that counts.
  - Store `source: engine_reviewer | human` and do not accept that field from the request body.
  - Have `review_ok` require one of those sources.
  - Have `review_ok` require that every blocking finding raised in *any* round is resolved, not
    only those in the last review.
  - Leave `submit_review` out of the MCP write catalogue, or mark reviews submitted over MCP as
    advisory.

### 5. Only the coder's prose is kept from the reviewer; nothing stops the coder changing tests — **high**

- **Location:** `engine.rs:682-744` (coder output can name any existing symbol, including
  tests); `context.rs:208-215` (`diff_md` skips `kind == test`); `context.rs:235-248`
  (`tests_md` labels every proposed test "NEW TEST" and shows no old body).
- **Problem:** The coder prompt says "not for tests", but the engine accepts a coder change to
  an existing test symbol (`sym` is any key in `g.symbols`). The coder can weaken a failing
  test, for example by dropping the locked-account assertion. The reviewer then sees that test
  as a "NEW TEST" with no old→new diff. `tests_md` shows the old body separately only if the
  test verifies a changeset requirement, so tests of other requirements that the change breaks
  are never shown. Doc 04's split (the tester writes tests from requirements, the coder writes
  code) is not enforced in code.
- **Recommendation:**
  - In `Engine::coder`, reject changes whose target is `kind == test`, or whose `kind` is test.
    Leave the test alone and feed the rejection back as a failure.
  - In `tests_md`, render modified tests as `MODIFIED TEST` with the old and new bodies.
  - Add an assertion to the M5 test that a recorded coder response touching a test is refused.

### 6. The `→ qa` gate checks coverage and orphans of the *working graph*, not the release — **high**

- **Location:** `crates/kodehub-agents/src/environments.rs:126-143`.
- **Problem:** `tests_gate` correctly runs the release graph (`release_graph`). But "no failing
  requirements" calls `self.store.coverage(None)`, and "no orphans" calls
  `self.store.orphans()`. Both read the *current* `symbols`, `current_version` and
  `test_runs`, and orphans are then filtered by key. So the gate answers a question about
  `main`, not about the release being promoted.
- **Scenario:**
  - Release R5 is good. A later merge to the working graph breaks a requirement. Promoting R5
    (or a hotfix release) to qa is now refused.
  - The reverse also happens. R5 contains a symbol that was an orphan when R5 was cut. A later
    changeset attaches a `satisfies` edge, so the current graph has no orphan, and R5 passes
    the gate with the orphan inside it.
  - Gates should be reproducible per release (D-028 says so).
- **Recommendation:** Compute coverage and orphans from the release snapshot. The release
  document already stores the code graph. Also snapshot the context edges at cut time:
  `satisfies`, `verifies`, `contains` to symbols, and the requirement revisions. Then evaluate
  coverage in memory against `test_runs` at the release's closure hashes. Add an M7 test where
  the working graph diverges from the release.

### 7. The QA "smoke run" for `→ prod` is a cache lookup, and prod approval is a caller-supplied string — **medium**

- **Location:** `environments.rs:145-149`; `routes/releases.rs:99-125`; `kodehub_core::tools`
  (`promote` tells agents to pass `approved_by: human`); `app/src/components/ReleasesDrawer.tsx:27-31`.
- **Problem:**
  - `tests_gate` calls `run_tests(.., force=false, ..)`. Every test passed at the same closure
    hash in the qa gate, so the "QA smoke run" re-runs nothing and does not use the qa
    environment's config.
  - Human sign-off is `approved_by == Some("human")`, which any HTTP or MCP caller can send.
    An external Claude Code session can promote to prod by itself, and the tool description
    even tells it the magic value. Doc 06: "Promotion into prod always requires human sign-off."
- **Recommendation:**
  - Run the smoke run with `force=true`, inside the qa environment's runtime (its
    `config.toml` and the qa release graph), and record its own `test_runs` with
    `environment: qa`.
  - Make approval a separate, UI-only step: `POST /api/environments/prod/approvals` creates a
    one-time approval id, and `promote` must quote that id.
  - Remove `approved_by` from the MCP tool schema, or reject `promote` to prod from MCP
    sessions.

### 8. Rollback ping-pongs instead of walking back — **medium**

- **Location:** `crates/kodehub-store/src/releases.rs:218-233`.
- **Problem:** Rollback picks the `previously_ran` edge with the latest `until`, then calls
  `deploy`, which turns the release being rolled back *from* into a new `previously_ran` with
  `until = now`. A second rollback therefore returns to the bad release.
- **Scenario:** test runs R1 → R2 → R3. A rollback goes to R2 (good). R2 is also broken, so the
  user rolls back again and lands on R3, the release they first rolled away from.
  `m7_releases.rs:169` only tests one rollback.
- **Recommendation:** Mark rollback deployments (`deployed_to.gate.rollback`, or a `rolled_back`
  flag on the `previously_ran` edge). Choose the most recent `previously_ran` release that was
  not rolled away from, or take an explicit target release. Add a two-step rollback test.
  Also do the "find previous" query inside the deploy transaction.

### 9. Changesets dead-end in `escalated` or in a blocked `ready` — **medium**

- **Location:** `crates/kodehub-core/src/model.rs:238-264` (`Escalated` is terminal);
  `engine.rs:491-497` (`Ready` → `merge` fails → error, and resume repeats the failure).
- **Problem:**
  - Doc 04 says "no consensus → escalated to the human", but the human can do nothing with an
    escalated changeset except abandon it. It is terminal, `ensure_editable` refuses edits, and
    the UI has no "accept finding / override / send back" action. Its requirements stay amber
    forever unless someone opens a new changeset by hand.
  - A changeset whose merge is blocked because the tests are stale (see #3: another merge
    changed a dependency) sits in `ready`. `POST /run` re-merges and fails the same way.
- **Recommendation:**
  - Allow `Escalated → Building | Testing` with a required human `Decision` (the human's
    resolution is the decision).
  - Allow `Ready → Testing`. When `merge_changeset` returns "tests missing", the engine should
    move to `testing` and re-run the tests rather than error.
  - Show both actions in the changeset card.

### 10. Merge silently drops `pinned_version` on the touched symbols' outgoing edges — **medium**

- **Location:** `changesets.rs:507-518` (the overlay removes every `calls`/`depends_on` from a
  proposal and re-adds the inferred edges with `pinned_version: None`); `:728-741` (merge
  deletes those edges and re-inserts them without data).
- **Problem:** Doc 02 makes pins first-class (`calls { pinned_version }`). If a touched symbol
  pinned a dependency, the merge un-pins it. The symbol then silently tracks the current
  version, and its closure hash changes meaning. The tests run on the overlay, which is also
  un-pinned, so nothing fails.
- **Recommendation:** When rebuilding the inferred edges, carry over `pinned_version` from the
  existing edge with the same (from, to, kind), both in the overlay and in the merge insert. Add
  a unit test.

### 11. Two changesets proposing the same new symbol: the second merge overwrites the first without a check — **low**

- **Location:** `changesets.rs:270-281` (a name lookup at propose time decides `new`);
  `:689-712` (at merge, `exists` → `write_version_in` with no `base_version` check because
  `new == true`).
- **Problem:** A and B both propose a new `dummy_password_hash`. A merges. B's merge sees
  that the symbol exists and replaces A's version. The display-name uniqueness rule only fires
  on the insert path.
- **Recommendation:** If `p.new == true` and the symbol now exists, return a `Conflict`
  ("re-propose against the current version").

### 12. Merge doesn't re-check state inside the transaction; reject and merge can race — **low**

- **Location:** `changesets.rs:663-670` (the state is read before the trx), `:826-829`
  (`state: merged` is patched without re-reading).
- **Problem:** A human rejects while the engine is merging, and the changeset still ends up
  `merged`, with the rejection lost from history (the merge writes its stale `history` array).
- **Recommendation:** Read the changeset inside the trx (it's in `MERGE_WRITE`) and abort
  unless it is `ready`. The same fix covers the RMW of `history`.

### 13. The coder isn't bound to the approved plan — **medium**

- **Location:** `engine.rs:682-744`.
- **Problem:** The human approves a plan, which the map shows as ghost nodes. The coder can
  then propose versions of *any* symbol, for example `hash_password` or an unrelated module.
  Only the reviewer might notice. Doc 04/05 make plan approval the human's main control point.
- **Recommendation:** When the coder touches a symbol that isn't in `plan.changes`, either
  reject the change or flag it (`out_of_plan: true`) and send the changeset back to `planned`
  for re-approval. Show out-of-plan proposals on the card.

### 14. The UI demo's "locked user" run passes for the wrong reason — **medium (acceptance)**

- **Location:** `app/e2e/demo.spec.ts:79-84`.
- **Problem:** Step 8 fills `password_hash` with a made-up Argon2 string
  (`c2FsdHNhbHQ$aGFzaGhh…`). `verify_password` fails on it, so `invalid_credentials` comes back
  whether or not the `locked` check exists. The test then saves this as a "locked user with the
  right password" example, which writes a mislabelled example into R1 (rev 3) and starts a new
  agent cycle (see #15). The server-side M5 test does it correctly (`m5_agents_demo.rs:222-235`
  hashes first). The UI acceptance for step 8 does not prove the locked path.
- **Recommendation:**
  - In the e2e, run `hash_password` in the app first and paste its result, or seed a real
    hash fixture.
  - Also assert that the same args with `locked: false` return `ok`.
  - Better for users: a "Use requirement example" picker in the run form, so the locked case
    doesn't need hand-built nested JSON (see UI section).

### 15. In recorded mode, an exhausted recording silently restarts and replays a stale plan — **low**

- **Location:** `engine.rs:117-125`.
- **Problem:** When the recording runs out, `backend()` loads the file again. In
  `07-history.png`, "Save as example" (rev 3) opens a new changeset whose plan again says "Add
  a `locked` flag to User…", although that change was merged a minute earlier. The demo ends
  amber, with a nonsensical plan waiting for approval.
  - Separately, Save-as-example always bumps the revision and starts a full agent cycle, even
    when the example duplicates an existing edge case.
- **Recommendation:**
  - In recorded mode, fail loudly when the recording is exhausted, or match responses by
    (role, label, changeset title) instead of by queue order.
  - Have Save-as-example check for an existing example or edge case with the same input and
    expected value, and skip the edit when one exists.

### 16. Origin allowlist accepts every localhost port, there is no Host check, and the embedded terminal is a full shell — **medium**

- **Location:** `crates/kodehub-server/src/lib.rs:46-67`; `routes/terminal.rs:37-71`.
- **Problem:** D-027 replaced permissive CORS with "any `localhost` / `127.0.0.1` origin on any
  port". The terminal WebSocket gives a login shell to any page served from *any* local port
  (another dev server, a notebook, or a local app with an XSS bug). The `Host` header is never
  checked, so DNS rebinding still allows same-origin GET reads (browsers omit `Origin` on
  same-origin GETs). That exposes code bodies, requirements and transcripts.
- **Recommendation:**
  - Allow only the configured UI origins (the Tauri origins plus the one Vite port), not every
    port.
  - Reject requests whose `Host` isn't `127.0.0.1:<port>` or `localhost:<port>`.
  - Require a per-launch random token for `/api/terminal`. The Tauri shell can inject it, and
    the server prints it at startup.

### 17. `Store::diff_releases` misses renames, visibility changes and edge changes — **low**

- **Location:** `releases.rs:94-125`.
- **Problem:** The diff compares only `version` per symbol. A rename or visibility change
  produces different rendered code (both feed the closure hash, D-007/D-027), and so does a
  re-pinned dependency. None of these change the version hash. The M7 claim "diff lists
  exactly the changed symbols" fails for them.
- **Recommendation:** Also compare `display_name`, `visibility` and each symbol's outgoing
  edges (or the `root_closures` already stored per release). Report these as `changed` with a
  `reason`.

### 18. Agent prompts are passed on argv — **low**

- **Location:** `backend/claude_code.rs:57-60`.
- **Problem:** `-p <task.input>` puts the whole prompt (requirements and code) on the command
  line. Other local users can read it with `ps`, and macOS `ARG_MAX` (about 1 MB including the
  environment) will fail large-context tasks with an opaque spawn error.
- **Recommendation:** Pipe the prompt on stdin (`claude -p` reads stdin when there is no
  positional argument), and keep only the flags on argv.

### 19. The orchestrator chat writes requirements without human confirmation — **low**

- **Location:** `engine.rs:1127-1161`.
- **Problem:** The LLM chooses `add_edge_case` / `create_requirement`, and the engine applies
  it at once with `by: "human (via orchestrator)"`. That attribution is wrong. It also lets
  prompt-injected text in referenced node content (included at `:1083-1087`) edit
  requirements, which then auto-open changesets. Vision: humans own requirements.
- **Recommendation:** Return the proposed requirement edit as a pending action that the UI
  shows with Apply/Discard. Apply it only on a human click, and attribute it to `orchestrator`
  until then.

### 20. M5 acceptance: the API-backend demo has never been run — **low (acceptance)**

- **Location:** D-030.
- **Problem:** The M5 criterion requires the demo to pass on the API backend. It was only
  mock-tested, which D-030 states honestly. The mock also doesn't check the structured-output
  parameter names against the live API.
- **Recommendation:** Keep M5 marked "partially accepted" until someone runs
  `KODEHUB_LIVE_BACKEND=api make live-test` with a Console key. Add a CI job that runs it when
  the secret is present.

### 21. The time slider doesn't show "the system as it was" — **medium (M6 feature)**

- **Location:** `crates/kodehub-server/src/routes/graph.rs:411` (seed time =
  `MIN(created_at)`), `:294` (nodes filtered by `created_at <= @at`), `:285`/`:316-317`
  (health is always *current*), `:326-337` (edges are always current), `:339-394` (ghost nodes
  are added even when `at` is set).
- **Problem:** `07-history.png` shows it. "As of Seed imported", the map has two nodes:
  `hash_password`, the first symbol the seed wrote (MIN timestamp), and the open changeset's
  *ghost* test. Both are drawn in today's health colours, and the top bar says "0 tests".
  Doc 05: "the map shows the system as it was".
- **Recommendation:**
  - Use `MAX(created_at)` for the seed event, or better, record one `seed_imported_at` on the
    import.
  - Skip ghosts when `at` is set.
  - In history mode, colour nodes by `test_runs` at the closure hash in force at `at`, or
    colour them neutral and show only `changed_since`.
  - Filter edges by `created_at`.
  - Add a Playwright assertion that the seed point shows every seed symbol.

### 22. UI health vocabulary is inconsistent — **low**

- **Location:** `app/src/lib/health.ts:5` (`uncovered` → amber, labelled "Untested");
  `SystemMap.tsx:341-346` (the legend has no "untested" entry).
- **Problem:** In `01-green.png`, the header and sidebar say "0 at risk", yet `check_password`
  (the planted duplicate, which has no tests) is amber: the legend's "at risk" colour. A user
  can't tell "untested" from "at risk", and the planted duplicate (doc 08: "removable after
  M3") still sits on the map, amber, after the demo goes green.
- **Recommendation:** Give `uncovered` its own style (amber outline with no fill, or hatched)
  and a legend entry. Remove the duplicate fixture from the seed, or tag it so the map hides it
  by default.

---

## UI review (M6) — look, feel and usability against doc 05

**Score: 6.5 / 10.**

**What works.** The visual language is coherent and calm: dark "mission control" theme,
consistent badges and colour semantics, glass overlays. The layout matches doc 05: pinned
mission banner, requirements left, map centre, agent team and changesets right, time slider and
environment bar at the bottom, and a dockable terminal. The requirement editor is good. The
anatomy is an editable form, "Add edge case" is one click and hinted ("how most bugs get
fixed"), and the history tab and "Changed: edge_cases" footer make revisions tangible. The
changeset card is the best piece: a lifecycle progress bar (plan → merge), modify/add chips per
symbol, risks, Approve/Reject, and a compact "9/9 tests · 2 review rounds · 1 decision" summary
once merged. The run form generated from the signature (nested `HashMap<String, User>` with
"Add entry") plus "cache hit · 29 ms" delivers vision point 4 visibly. The releases drawer
(DEV/TEST/QA/PROD pipeline, gate checklist, select-two-to-diff) is clear.

**What holds it back.**

1. **Map legibility (biggest issue).** At default zoom the symbol labels are about 7 px
   (`01-green.png`, `04-merged-green.png`). The map uses about 40% of its pane, and doc 08's
   cast of nine symbols is barely readable. Semantic zoom starts at depth 4 (everything
   expanded, `SystemMap.tsx:134`) instead of "zoomed out shows contexts". Fix: start at the
   component level and expand on zoom or double-click. Set Cytoscape `min-zoomed-font-size`,
   use a layout that fills the viewport, and hide `function · rs` sub-labels below a zoom
   threshold.
2. **Ghost nodes are only half there.** New symbols are dashed (good), but *modified* symbols
   (`User`, `verify_credentials`) get only a faint outline you can barely see
   (`03-planned-ghosts.png`). Doc 08 step 3 names both kinds. Give modified nodes a dashed
   violet halo and a "Δ" glyph, and link plan rows to map nodes on hover.
3. **The time slider is broken in practice** (finding 21). The two-handle track also gives no
   labels: a stop has to be hovered before you know what it is.
4. **The node panel covers the map.** Opening `verify_credentials` hides the very nodes you
   clicked (`05-run-now.png`, `node.png`). Dock it as a fourth column, or shift the map
   viewport left, and keep the selection visible.
5. **Run now needs domain knowledge.** To run the locked-user case, a person must run
   `hash_password`, copy the hash, and build a nested store by hand. Offer "Fill from
   requirement example/edge case" and remember the last args per symbol. This is what makes
   step 8 doable "by a person" (M6 acceptance).
6. **Toasts pile up and go stale.** "R1 updated…" is still on screen after the merge
   (`04-merged-green.png`), and three toasts cover the map centre in `06`/`07`. Auto-dismiss
   after about 5 s, stack them bottom-right, and keep one per topic.
7. **The agent panel lacks doc 05's to-do lists and confidence flags.** Agents show only
   "idle · last: plan". Reviewer findings and the decision are behind "Details" and scrolling
   on merged cards. Show the current task's checklist and surface open blocking findings on the
   card face.
8. **No path forward from escalated or blocked changesets** (finding 9). The card has no
   "resolve / override / send back" actions.
9. **Mixed signals:**
   - `at_risk` and `uncovered` share a colour (finding 22).
   - The header test count goes to "0 tests" in history mode.
   - "Recorded (demo)" and the model name sit in the status bar with no indication that
     recorded mode replays canned answers (finding 15).

**What would raise it to 8–9:** items 1, 2, 4 and 5 above (map legibility, real ghosts for
modifications, a non-occluding node panel, example-driven run forms). Then a working time
slider with labelled stops, and escalation actions on the changeset card. Toast hygiene and
agent to-do lists are quick wins.

---

## Summary

M5–M7 are implemented end to end and the demo really runs. In recorded mode it runs in CI, and
on the Claude Code backend it ran live (D-030). The engine is resumable and state-driven, agent
writes go through the graph API (D-025), blind review is structural (the engine builds every
reviewer prompt, and the reviewer's MCP tool set is `list_requirements` only), merges are one
ArangoDB transaction with the orphan check inside it, and releases plus `currently_running`
lean on a unique index.

The weaknesses cluster around **invariant 6 and the "green" signal**:

- `at_risk` is cleared by *any* merge that lists the requirement, including an empty one or an
  out-of-date one (#1).
- A "passing blind review" can be posted by any API or MCP caller (#4).
- The coder can rewrite tests, and the reviewer then can't see a test diff (#5).
- Merge checks run outside the transaction, and concurrent merges aren't serialised (#3).
- `transition` can mark a changeset merged without merging it (#2).
- On the release side, the qa gate evaluates the working graph rather than the release (#6),
  the prod "human approval" is a string anyone can send (#7), and rollback ping-pongs (#8).
- Workflow dead-ends (#9: escalated and blocked-ready) will hurt as soon as real models
  disagree.
- The UI is coherent and the changeset and requirement flows are good. It is held back by
  map legibility, a broken time slider (#21) and a run form that needs hand-built fixtures. Its
  step-8 e2e passes for the wrong reason (#14).

**Most valuable fixes, in order:**

1. Revision-aware `at_risk` plus a "requirement covered by a new test" merge check (#1).
2. Server-attested reviews, with a test-edit ban for the coder (#4, #5).
3. A merge mutex, in-trx checks, and the `merged` transition restricted to merge (#2, #3).
4. Release-scoped qa gates and a real prod approval (#6, #7).
5. Rollback history (#8) and escalation actions (#9).
6. Map legibility and time-slider fixes in the UI (#21).

Note for #2: the UI's manual-edit flow (`NodePanel.tsx:81-87`) relies on the generic
`transition` route, so restricting that route needs a dedicated
`POST /api/changesets/{k}/manual-edit` endpoint.

---

## Things done well

- **The state-driven, resumable engine** (`engine.rs:392-504`), with max-round escalation and a
  per-changeset running guard. It is easy to reason about.
- **Blind review by construction.** The reviewer input is assembled only from the mission,
  requirements, diff and tests (`context.rs:208-265`), it has a one-tool MCP allowlist, and the
  M5 test asserts no coder text reaches it.
- **The tester sees signatures and data shapes, never function bodies** (`code_md(.., false, ..)`),
  and this is tested.
- **Merge integrity:** `base_version` checks, the proposal fingerprint on reviews, and the
  orphan check inside the same stream transaction with dry-run reuse for `/checks` (D-024).
- **Releases:** immutable manifests carry the full code graph, so run-now in qa or prod and
  bisect execute the release's own code. `deploy` moves `currently_running` →
  `previously_ran` in one transaction behind a unique index.
- **Credentials:** `claude-as.sh` is the only code that dereferences a token. The API backend
  refuses `sk-ant-oat` tokens. The PTY and agent children strip `ANTHROPIC_API_KEY`.
- **UI:** the requirement editor and the changeset card make the "humans refine requirements,
  agents do the rest" loop tangible. Real Playwright e2e screenshots for every demo step are
  excellent evidence.

