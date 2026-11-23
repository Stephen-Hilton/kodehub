# Critical review — Kodehub pilot at completion (M8 + verification of D-036/D-037)

Reviewer: staff-level critical review (automated agent, claude-opus-5-5). Scope: M8 (PyO3 binding),
verification of the fixes claimed in D-036/D-037, UI re-score, per-milestone verdict. Findings from
`review-m4.md` / `review-m6.md` are not repeated unless the claimed fix is wrong or incomplete.
This is a reading review: I did not run `make check` or the tests.
**Note:** files changed while I was reviewing. For example, `BuildTarget::Lib` gained a
`namespaces` field between two reads of `build.rs`. Line numbers reflect the tree as I read it.

## Summary

_(written last — see end of file)_

---

## Findings

### M8 — PyO3 binding

#### 1. The extension artifact is copied from a shared, fixed path, which is the same bug class D-036 fixed for `kodehub_run` — **high**
- **Location:** `crates/kodehub-runner/src/build.rs:333-357` (`target/debug/libkodehub_ext.{dylib,so}`
  in the shared `CARGO_TARGET_DIR`); `crates/kodehub-runner/src/lib.rs:43,61` (`build_lock` belongs
  to each `Runner` instance, not to the process or the target dir).
- **Problem:** Every extension crate has the same library name, `kodehub_ext`, which the
  `PyInit_kodehub_ext` symbol requires. Cargo copies each build's output to the same
  `cargo-target/debug/libkodehub_ext.dylib`. The copy into the package happens after `cargo build`
  returns, and only an in-memory mutex of one `Runner` protects it. Two `Runner`s share the target
  dir in several cases: the server and a test binary, two tests that each call `runner()`, or
  several homes sharing `KODEHUB_CARGO_TARGET` (`lib.rs:73`). In each case closure X can receive
  closure Y's `.so`, and that `.so` is then cached under X's key in `artifact.json` for good.
  D-036 shows this exact failure already happened once with `kodehub_run`, and it was fixed with
  per-closure bin names (`bin_name`). The extension path brings the shared name back.
- **Recommendation:** Build each extension with its own target dir
  (`CARGO_TARGET_DIR=<cargo-target>/ext/<closure16>`). Dependencies are rebuilt once per closure,
  which is acceptable for a stretch goal. Alternatively, take an OS file lock (`flock` on
  `cargo-target/.kode.lock`) around build + copy, for both the Rust and the ext paths, and make
  `build_lock` process-global. Add a test that builds two different ext closures concurrently
  with two `Runner`s and checks the loaded module behaves differently.

#### 2. Transient extension-build failures are cached as permanent failures — **medium**
- **Location:** `build.rs:261-265` returns `cacheable = true` for *any* `build_ext` failure;
  `build_ext` returns `ok=false` for a `cargo fetch` network failure (`:326-331`), a venv failure
  (`:311-315`) and a build timeout (`:342-344`).
- **Problem:** `build_rust` carefully avoids caching fetch failures and timeouts
  (`:199-208`, `:236-243`). The Python path, which wraps the extension build, writes
  `artifact.json {status: failed}` whatever the cause. One offline moment or a slow first PyO3
  compile (≤900 s) makes run-now of the Python wrapper fail for that closure until someone deletes
  `.kode/cache` by hand.
- **Recommendation:** Have `build_ext` return the same `(ok, log, deterministic)` triple as
  `build_rust`, and pass `deterministic` through (cache only `error[E…` / `could not compile`).

#### 3. `bound_rust_symbols` accepts any Rust symbol kind, and the shim and extension can disagree on the version — **low**
- **Location:** `crates/kodehub-render/src/lib.rs:212-229`; `rust.rs:375-405`; `python.rs:50-62`.
- **Problem:**
  - A `binds_to` edge to a class, enum, method (`User::check`) or generic fn renders
    `pub fn User::check(…)` or `super::User(…)`, which won't compile. The edge API accepts such
    edges, and the error only appears as a cached "PyO3 extension build failed" log.
  - The Python shim takes parameter names from `g.symbols[b].version`, the *current* version
    (`python.rs:52`). The extension uses the *resolved* member version (`rust.rs:377`). With a
    `pinned_version` on the `binds_to` edge, positional args are zipped to the wrong names.
  - If a Python symbol has the same display name as the bound Rust fn (names are unique per
    *language* only), its `def` silently shadows the shim.
- **Recommendation:** At render time, refuse `binds_to` targets that `runnable()` would refuse,
  with a clear error. Use `p.members[b]` in the shim. Refuse a Python/Rust name clash for bound
  names.

#### 4. M8 test proves the happy path but not the "through the extension" claim or the cache paths — **low**
- **Location:** `crates/kodehub-runner/tests/m8_pyo3_binding.rs`.
- **Problem:**
  - Nothing asserts that the result came from Rust. `login` could pass the four tests and the run
    if a Python `verify_credentials` were in scope (see #3). One `kodehub_ext` import assertion, or a
    Rust-only behaviour such as the panic → `PanicException`, would make it airtight.
  - The second run-now (`:74`) is a cache hit, but the test checks neither `cache_hit` nor its
    timing. The test path (`testing.rs:365`) rebuilds the extension on every batch (no cache), and
    nothing covers that.
  - The signature-change step (`:102-116`) asserts `at_risk` only via closure-hash staleness. It
    doesn't re-run the tests to show that the wrapper still works, or fails, against the new
    signature. That's acceptable for the criterion as written.
  - The binding is wired by hand in the test. The seed has no `binds_to` edge, so the binding
    never appears in the app, the map or the e2e.
- **Recommendation:** Assert `import kodehub_gen.kodehub_ext` in the test render, assert
  `cache_hit` on the second run, and cache the ext build by its own closure
  (`cache/<rust-closure>/python-ext-<tag>/kodehub_ext.so`). That cache would also be the
  `python-ext` build target D-034 names but `BuildTarget` lacks (`build.rs:18-26`). Optionally add
  the binding to the seed as a second scenario.

#### 5. The extension cache key ignores the Rust toolchain — **low**
- **Location:** `lib.rs:91-111`. `python-pkg`/`python-run` tags hash only `python --version`.
- **Problem:** A Python artifact that embeds a compiled Rust extension is reused after a `rustc`
  upgrade. The PyO3 version is a caret requirement (`rust.rs:352`) and there is no lockfile, so
  the D-027 "not done" lockfile caveat now affects the Python targets too.
- **Recommendation:** If the rendered project has `ext`, include the Rust tag in the Python tag.

#### 6. Namespace re-exports are not in the build-cache key — **medium** (code changed during the review)
- **Location:** `build.rs:18-24,97-108` (`BuildTarget::Lib` now carries `namespaces`);
  `build.rs:130-135` (the cache dir is `closure_hash` + `t.name()`); `kodehub-render/src/lib.rs:144`
  (the closure hash covers the roots only).
- **Problem:** The `→ test` gate (`environments.rs:58-74`) now renders `pub mod auth_sdk { pub use … }`.
  Adding, renaming or removing a namespace export changes the rendered crate but not the key. The
  gate then reports a cached "build ok" for code that was never compiled, or keeps reporting a
  cached failure after the fix. This is the same class as the D-036 `GENERATOR` bug.
- **Recommendation:** Fold a hash of the sorted `namespaces` into the cache dir name, or into the
  manifest's closure hash for `Lib` targets. Extend
  `renderer_changes_bump_the_generator_version` to cover the case.

---

### Verification of D-036 (fixes for review-m6)

| # | Claim | Verdict |
|---|---|---|
| 1 | Revision-aware `at_risk` + `requirements_covered` | **Partial**: see #7 (a second door to a false green, and a permanently stuck amber) |
| 2 | Generic transition restricted | **Real** (`routes/changesets.rs:204-216`). `merged` is only set inside merge. `approved` is still open to any caller (#8) |
| 3 | Merge/deploy write lock, in-trx re-read | **Real for merge/deploy** (`changesets.rs:741,781`). Other graph writers skip the lock (#9) |
| 4 | Attested reviews | **Incomplete**: "human" means the request carried an allowed `Origin` header, which any local process can send (#8) |
| 5 | Coder can't change tests; reviewer sees MODIFIED TEST | **Real** (`engine.rs:777-786`). The name-lookup path leaves a small gap (#12). No test asserts the refusal |
| 6 | qa gate on the release snapshot | **Real** (`releases.rs:217-323`), but `unverified`/`at_risk` are ignored (#10). No divergence test |
| 7 | Forced smoke run, one-time approval | **Real** (`environments.rs:148`, `routes/releases.rs:121-132`). The approval id is guessable, and the human check is forgeable (#8) |
| 8 | Rollback walks back | **Real** (`releases.rs:411-433`). The test only exercises "nothing older". Releases rolled away from are blacklisted forever (#13) |
| 9 | Escalation resolve; stale `ready` → `testing` | **Real** (`routes/changesets.rs:435-535`, `engine.rs:513-529`). Matching on the string `"tests_ok=false"` is fragile. Untested |
| 10 | Pins survive overlay and merge | **Real** (`changesets.rs:533-554,843`). Untested |
| 11 | New-symbol clash at merge | **Real** (`changesets.rs:809-814`) |
| 12 | Concurrent reject wins | **Real** (`changesets.rs:781-784,948`) |
| 13 | Out-of-plan → re-approval | **Round 0 only** (#11) |
| 15 | Exhausted recording fails loudly | **Real** (`engine.rs:117-127`, `m5_agents_demo.rs:262`) |
| 16 | Exact origins, Host check, terminal origin | **Real** (`lib.rs:53-131`, unit-tested). Missing `Host` is allowed through, which is acceptable |
| 17 | Diff reasons | **Real** (`releases.rs:163-180`). No test asserts `reasons` |
| 18 | Prompt over stdin | **Real** (`claude_code.rs:57,107-120`) |
| 19 | Chat proposals need Apply | **Real** (`engine.rs:1252-1330`). But MCP still exposes `update_requirement`/`create_requirement` directly (#8) |
| 21 | Time slider | **Real** (`graph.rs:294-300,343,420`) |

"All 22 findings were accepted. Implemented" overstates the test evidence. Only #1(b), #2, #4,
#7 and #15 have automated assertions (`m5_changeset_rules.rs`, `m7_releases.rs`,
`m5_agents_demo.rs`). #3, #5, #6, #9, #10, #13, #17 and #19 have none. CLAUDE.md asks for
invariants to be "enforced in code, tested in code", and invariant 6 is exactly what these fixes
protect.

#### 7. A false green still has a second door, and amber can become permanent — **high**
- **Location:** `changesets.rs:699` (`requirements_covered` applies only when
  `cs.origin == "requirement_edit"`); `routes/changesets.rs:358-390` (manual edit opens a
  changeset over *every impacted requirement*, with no `origin`); `changesets.rs:938-946` (the merge
  clears `at_risk`); `changesets.rs:970-981` (reject leaves the flag on, and the loop is a no-op).
- **Scenario A (false green):** The human adds an edge case to R1 (rev 2, at risk). The
  auto-opened plan isn't what they want, so they reject it. They then hand-edit
  `verify_credentials` in the text view. The manual-edit changeset addresses R1 at rev 2, has no
  `origin`, needs no verifying test, passes the old tests and the blind review, and merges. R1
  turns green with **no test for the new edge case**. That is review-m6 #1 again through another
  door. Any changeset opened over HTTP/MCP with `requirements: [r1]` behaves the same, and so does
  every requirement edit when the engine isn't configured (`lib.rs:195-204`), because then no
  changeset gets the `origin`.
- **Scenario B (stuck amber):** cs1 addresses rev 2 and cs2 addresses rev 3, and both are open.
  cs2 merges first, and cs1 is still open, so the flag stays. cs1 then merges, and rev 2 ≠ 3, so the
  flag stays. R1 is amber forever with both edge cases covered. An abandoned draft, or an
  escalated changeset listing R1, blocks the flag the same way, and nothing ever re-evaluates it.
- **Recommendation:** Tie coverage to the requirement, not to the changeset's origin:
  - Store `covered_revision` on the requirement.
  - Any merge that lists a requirement whose `revision > covered_revision` must include a
    passing proposed test that `verifies` it, and then sets `covered_revision = addresses[r]`.
  - Derive `at_risk = revision > covered_revision ∨ open changesets on r` instead of keeping a
    flag, and recompute it on merge *and* on reject.
  - Test both scenarios.

#### 8. "Human-only" means "sent an allowed `Origin` header", which any local process can send — **high**
- **Location:** `lib.rs:96-104` (`from_ui`), used by `submit_review` (`routes/changesets.rs:168`),
  `resolve` (`:441`), `approve_prod` (`routes/releases.rs:172`), `apply_chat` (`routes/agents.rs:77`)
  and the terminal (`routes/terminal.rs:44`). There is no check at all on `approve` / `transition →
  approved` (`routes/changesets.rs:97-115,204`), `rollback` (`routes/releases.rs:151`) or the
  MCP `update_requirement` / `create_requirement` tools (`kodehub-core/src/tools.rs:38-43`).
- **Problem:** `Origin` protects against *browsers*. Review-m6 #4 and #7 named a different threat:
  a Claude Code session in the embedded terminal, which has a shell (doc 05). Any non-browser
  client can set the header to `http://localhost:1420`. That makes its review `source: human`,
  mints a prod approval, resolves escalations, or applies chat proposals. Plan approval, "the
  human's main control point", needs no header at all. So D-036 #4 and #7 are fixed against an
  MCP-only agent, not against the agent the review described. The approval id is also only
  `format!("{nanos:x}")` (`routes/releases.rs:195-201`), which is predictable.
- **Recommendation:**
  - Generate a per-launch random secret at server start. Hand it only to the UI, as an `HttpOnly;
    SameSite=Strict` cookie set when the server serves the UI, or through a Tauri command. Require
    it on every human-only route, including `approve`, `transition`, `rollback` and requirement
    edits.
  - Never put the secret in the PTY's environment.
  - Make the MCP requirement tools create pending proposals, the same as chat.
  - Use `uuid::Uuid::new_v4()` for approval ids. The crate is already a dependency (`releases.rs:27`).
  - Add a test that a request with a forged Origin and no secret gets 403.

#### 9. Code-graph writes that bypass changesets entirely — **high**
- **Location:** `routes/mod.rs:35-39` → `store.rs:247-281` (generic `POST/DELETE
  /api/edges/{calls|depends_on|binds_to|tested_by|member_of|verifies|satisfies}`);
  `store.rs:230-243` (`PATCH /api/nodes/symbols/{k}` allows `visibility`); `routes/graph.rs:163-167`
  (`POST /api/seed` with any server-side path writes versions directly).
- **Problem:** Invariant 6 and the green signal depend on code changing *only* through a
  changeset merge. These routes change what is built and tested on `main` with no tests, no
  review, no orphan check and no `write_lock`. Deleting a failing test's `verifies` /
  `tested_by` edge turns its requirement green. Adding a `calls`/`binds_to` edge changes closures
  under a merge that is checking them, which breaks the lock's guarantee (#3). A `visibility` patch
  changes rendered code and, by D-027, the closure hash, but it does **not** call
  `refresh_closure_hashes`. The stored `symbols.closure_hash` goes stale, and coverage keeps
  reporting passes for a closure that no longer exists. The M8 test itself wires its binding this
  way (`m8_pyo3_binding.rs:16-39`). The UI only uses `/api/seed` (`app/src/lib/api.ts:144`).
- **Recommendation:**
  - Refuse code and trace edge collections (`CODE_EDGES` + `verifies` + `satisfies`) in
    `check_edge_collection` for the generic API, so they go through `propose_edge` on a changeset.
  - Drop `visibility` from the patchable symbol fields, or refresh the closure hashes under the
    write lock.
  - Take `write_lock` in seed import and refuse to re-seed over a non-empty graph unless forced.

#### 10. The `→ qa` gate ignores requirements that are at risk or unverified in the release — **medium**
- **Location:** `releases.rs:300-309` computes `unverified_requirements`, and `cut_release`
  snapshots `at_risk` (`:47`). `environments.rs:140-144` checks only `failing_requirements` and
  `orphans`.
- **Problem:** A release cut right after a human adds an edge case (R1 at risk, no test yet)
  passes `→ qa`, because the old tests still pass. The map and coverage say amber while the
  pipeline says green. The same goes for a requirement with zero tests.
- **Recommendation:** Add a "no requirement at risk or unverified at cut" check, or at least show
  it as a warning on the gate report. Also take `write_lock` in `cut_release`
  (`releases.rs:23-54`). Without it, the graph and the trace snapshot are read separately, and a
  concurrent merge can leave them inconsistent.

#### 11. The out-of-plan check runs only in round 0 — **medium**
- **Location:** `engine.rs:441` discards `coder()`'s return value during rework; `engine.rs:846`
  (`if initial && !in_plan`).
- **Problem:** Rework after "changes requested" is exactly when a coder widens scope, for
  example by "hardening" `hash_password` as well. Those proposals reach review and merge with no
  human re-approval. D-036 says "Round-0", which is honest, but review-m6 #13's concern stands.
- **Recommendation:** Check every round against `plan.changes` ∪ the already re-approved
  out-of-plan set, and send the changeset back to `planned` the same way.

#### 12. The coder's test ban misses the name-lookup path, and kind mismatches surface only at merge — **low**
- **Location:** `engine.rs:777-786` checks the *resolved key* or `kind == "test"`;
  `changesets.rs:293-301` resolves `symbol: ""` + `display_name` to an existing symbol and keeps
  the caller's `kind`.
- **Problem:** A coder change `{symbol: "", display_name: "test_locked_user_is_invalid", kind:
  "function"}` passes the ban. It becomes a modification of the test symbol, and the overlay
  renders it as a **test** with the coder's body (`overlay_graph` keeps the symbol's kind). Tests
  pass, review runs, and then `write_version_in` refuses the kind mismatch at merge
  (`store.rs:423`). The changeset dead-ends in `ready` with a non-`tests_ok` error. Integrity
  holds, but the weakened test ran.
- **Recommendation:** In `propose_version`, refuse a `kind`/`language` that differs from the
  existing symbol. In the engine, apply the test ban after name resolution.

#### 13. Rollback blacklists a release forever — **low**
- **Location:** `releases.rs:417` (`bad` = every release ever rolled away from in this env).
- **Problem:** Suppose R2 is rolled back because of a flaky environment issue, then later
  re-promoted through the gates. It can still never be a rollback target again, so a later
  rollback skips past a known-good release.
- **Recommendation:** Only consider rollbacks *since the target's latest deployment*, or have
  rollback accept an explicit target release.

#### 14. The generator-bump golden test does not cover the PyO3 or namespace output — **low**
- **Location:** `kodehub-render/src/tests.rs:260-290`. The sample graph has no `binds_to` edge,
  and the hashed request uses `namespaces: vec![]`.
- **Problem:** The D-036 safety net ("fails whenever rendered output changes without a version
  bump") is blind to `render_ext` (`rust.rs:356-417`), the Python shim (`python.rs:44-65`) and
  namespace modules. The next change there can reuse stale `.so` files under the same key, the
  same failure D-036 describes.
- **Recommendation:** Add a `binds_to` edge and a namespace to the golden sample, and include
  `project.ext.files` in the hash.

#### 15. Save-as-example duplicates the edge case and leaves the demo amber with a stuck draft — **low (acceptance hygiene)**
- **Location:** `app/e2e/demo.spec.ts:106-110`; review-m6 #15, second bullet (the dedupe was not
  done); `e2e/06`–`08` screenshots.
- **Problem:** The step-8 example ("locked user with the right password → invalid_credentials")
  repeats the step-2 edge case. It still bumps R1 to rev 3 and opens a draft changeset. With the
  recording exhausted, that draft can't plan. Doc 08 ends at "map returns to green … promote to
  `test`", but the recorded demo ends at **1/2 passing, R1 at risk, a dead draft**. That same
  amber release then passes the `→ test` gate (see #10).
- **Recommendation:** Skip the requirement edit when an example or edge case with the same input
  and expected value exists. Otherwise record a third response set for the rev-3 cycle so the demo
  ends green.

---

## UI review (M6) against doc 05, using the current screenshots

**Score: 7.5 / 10** (was 6.5).

**What improved, verified in `e2e/01`–`08`, `v2-*` and `v3-*`:**
- **Map legibility.** At 1600 px the doc 08 map fills the pane with readable ~10 px labels
  (`01-green`, `04-merged-green`).
- **Proposals.** Modified symbols show a dashed violet border plus `Δ` (`Δ User`,
  `Δ verify_credentials`), and the new test is a dashed ghost in its own column
  (`03-planned-ghosts`).
- **Legend.** It now has *untested* and *new · Δ changed*, and *changed since then* in history
  mode.
- **Node panel.** It is docked, and the map re-centres on the selection (`v2-node`, `05-run-now`).
- **Run now.** It has "Fill from a requirement example…", a Copy button, and "cache hit · 20 ms".
  The locked-user run now uses a real Argon2 hash produced in the app, so step 8 is honest.
- **Agents.** Each agent shows a to-do checklist ("✓ plan ● your approval ○ decisions ○ merge").
  The merged card shows confidence chips ("review r2: approved · 1 finding resolved ·
  9/9 tests · 1 decision"). The "demo replay" badge removes the recorded-mode confusion.
- **Time slider.** Stops are labelled. "Viewing the system as of Seed imported" shows every seed
  symbol in neutral colours with changed nodes highlighted and no ghosts (`07-history`).

**What still holds it back:**
1. **Layout overflow when the node panel is docked.** In `05-run-now` the whole app is shifted
   left: the logo, "EQUIREMENTS" and "nvironment" are clipped. In `v2-node`/`v3-node` the map
   is cropped on the left and not re-fit. `v3-node` shortens the zoom tabs to **"Cont" / "Cont" /
   "Comp"**, so Contexts and Containers become indistinguishable. Fix: give the map column a
   `min-width: 0`, let the panel take width from the map rather than the page, re-fit after the
   resize, and use icons or "Ctx/Ctr/Cmp" consistently.
2. **Toasts occlude actions.** In `03` a toast sits over the plan rows directly above *Approve
   plan*. In `06`–`08` three toasts cover the changeset card and the terminal's **Connect Claude
   Code** button. Move toasts to the map's bottom-left, or clear of the right sidebar.
3. **Legibility still depends on space.** With the terminal open (`08-terminal`) labels drop to
   about 6 px. `small3.png` (1280×800) and `insights.png` predate D-037: their legend lacks
   "untested" and the duplicate fixture is still on the map. So there is no current evidence for
   laptop-size screens. Regenerate them. Hide sub-labels, or collapse to components, when the
   pane is short.
4. **The requirement editor is a cramped modal.** Edge-case input and expected values are
   single-line, truncated JSON fields (`02-edge-case`: `{"password":"wrong","username":'`). The
   modal also hides the map, so the human can't see what the edit puts at risk. Use a small
   key/value form, or a multi-line field, and a side sheet instead of a full-height overlay.
5. **Run now for `verify_credentials` still needs two runs and a paste.** The requirement
   examples have no `store`, so "Fill from example" can't build one. Offer a "demo store"
   fixture, or bind the examples to `login()` (the M8 wrapper over `demo_users`), which takes only
   username and password.
6. **End state.** The demo finishes amber with a stuck rev-3 draft (#15). Timeline labels are
   truncated ("R1 — Generic failure (rev").
7. **Not shown in any screenshot:** the escalation actions (Send back / Accept & re-test),
   out-of-plan re-approval and chat Apply/Discard. The e2e doesn't exercise them either.

**To reach 8.5–9:** fix 1 and 2 (layout overflow and toast placement), give the requirement
editor a real input form (4), add a store fixture for run now (5), and make the demo end green
(6). Then add Playwright steps for escalation and chat Apply.

---

## Milestone readiness (M0–M8)

| Milestone | Verdict | Reason |
|---|---|---|
| M0 Foundations | **Accepted** | Workspace, compose, `make check` and `/health` with DB status are in place. I did not re-run them. |
| M1 Graph core | **Accepted** | Golden hashes, idempotent re-seed, one-version-per-change, closure propagation and the unique-index invariant are all tested (`m1_graph_core.rs`, `golden_hash.rs`). Caveat: a symbol `visibility` patch leaves stored closure hashes stale (#9). |
| M2 Render/build/run | **Accepted** | Byte-identical renders, cache hit < 1 s, dependents-only rebuild and the 10 s kill are tested (`m2_render_build_run.rs`). New caveat: namespaces are missing from the cache key (#6). |
| M3 Tests & traceability | **Accepted** | Coverage, orphans, impact, duplicates and red propagation are tested on seed data (`m3_tests_traceability.rs`). |
| M4 API & MCP | **Accepted** | The full seed scenario runs over HTTP (`m4_http_api.rs`), MCP over stdio, and live Claude Code over MCP (D-030). |
| M5 Agents & changesets | **Partially** | Recorded and live Claude Code demos pass, and blind review is asserted. But: (a) the API backend demo has never run live (D-030, review-m6 #20 still open). (b) Invariant 6's "passing blind review" and the human controls can be forged by any local process (#8), and code edges can bypass changesets (#9). (c) A false green is still reachable through manual edits or origin-less changesets (#7). |
| M6 Desktop UI | **Accepted** | The Playwright e2e performs the doc 08 demo in the app (seeding through the API is setup, not CLI), step 8 is now honest, and the UI scores 7.5/10. The remaining issues are polish (UI section) plus the amber end state (#15). |
| M7 Releases & envs | **Accepted** | Immutability, one `currently_running` per env, rollback and diff are tested (`m7_releases.rs`). Caveats: the qa gate ignores at-risk requirements (#10), the prod approval is forgeable (#8), and there is no test for the gate/working-graph divergence or for diff `reasons`. |
| M8 PyO3 binding | **Partially** | The criterion is met in `m8_pyo3_binding.rs`: `login()` → Rust through the generated extension, and a Rust signature change → wrapper `at_risk`. But: the extension artifact is copied from a shared fixed path, which is the cache-poisoning class D-036 fixed (#1); transient failures are cached (#2); and doc 08's "M8: `binds_to` the Rust function" is not in the seed, so the binding exists only inside one test (#4). |

---

## Summary

The pilot is complete end to end, and most of the D-036/D-037 fixes are real and well placed:
- `merged` is only reachable through `merge_changeset`, which holds a write lock and re-reads
  the changeset inside the transaction.
- The qa gate reads a release-scoped trace snapshot.
- The prod smoke run is forced, and approvals are one-time.
- Rollback walks back.
- Pins survive the merge.
- Prompts go over stdin.
- Chat proposals need Apply.
- The time slider now shows real history.

The UI moved from 6.5 to **7.5/10**. The map is legible, modifications look different from
additions, the node panel is docked, run now fills from examples, and agents show to-dos and
confidence flags.

The remaining risk is concentrated in one theme: **who and what can make the system say
"green"**.
- "Human" attestation is an `Origin` header any local process can send, and plan approval,
  rollback and MCP requirement edits have no human check at all (#8).
- The generic edge API and symbol patches change code and trace edges on `main` without any
  changeset (#9).
- `requirements_covered` only applies to changesets the engine opened with
  `origin: requirement_edit`, so a manual edit can still clear an edge-case edit with no test.
  Meanwhile, two overlapping changesets can leave a requirement amber forever (#7).
- The qa gate passes releases whose requirements are at risk (#10), which is exactly what the
  recorded e2e demo does (#15).

M8 meets its criterion, but it reintroduced the shared-artifact-path bug D-036 had just fixed
(#1) and caches transient failures (#2). Also, D-036's "all 22 implemented" is supported by tests
for only about a third of the items.

**Most valuable fixes, in order:**
1. Per-launch UI secret on every human-only route (#8).
2. Close the generic code/trace edge and visibility write paths (#9).
3. Requirement-centric `covered_revision` (#7).
4. Per-closure target dir, or a file lock, for the PyO3 build, plus determinism-aware caching
   (#1, #2).
5. At-risk check in the qa gate, and the write lock in `cut_release` (#10).
6. Tests for the untested D-036 items.
7. UI layout overflow and toast placement.

---

## Things done well

- **The D-036 fixes are placed at the right choke points.** Merge re-reads state inside the
  transaction (`changesets.rs:781`). Attestation `source` is set server-side and never taken from
  the request body (`changesets.rs:440-443`). Blocking findings must be resolved across *all*
  rounds (`:676-686`). The test-edit ban sits in the engine.
- **The M8 design is pragmatic.** JSON across the PyO3 boundary avoids per-type conversions,
  `abi3` gives one build per Python, and `binds_to` as a closure edge makes "Rust signature change
  → Python wrapper at risk" fall out of the existing closure-hash machinery for free.
- **Releases are reproducible.** The trace snapshot on the release document gives
  release-scoped gates without new collections. Rollback marks the `previously_ran` edge, and
  `currently_running` keeps its unique index.
- **The UI is honest.** The "demo replay" badge, labelled time stops, neutral history colours,
  and the step-8 e2e that hashes a real password and proves both the unlocked and locked paths.
- **The decision log is candid.** D-030, the round-0-only caveat in D-036 #13, and the
  self-reported `GENERATOR` cache bug make review much easier.
