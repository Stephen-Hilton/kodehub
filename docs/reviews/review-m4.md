# Critical review — Kodehub pilot, milestones M0–M4

Reviewer: staff-level critical review (automated agent). Scope: `CLAUDE.md`, `docs/00`–`08`,
`docs/decisions.md`, `crates/kodehub-{core,store,render,runner,server,cli}`, `examples/auth-poc/`.
`crates/kodehub-agents` was being edited during the review (M5 work, e.g. `engine.rs` appeared
mid-review), so I only read it for context. I could not confirm that `make check` is green on the
current tree.

## Summary

The core is in good shape for a pilot. Content addressing, the versioning transaction, the
unique-index enforcement of invariant 1, deterministic rendering, the closure-hash definition
(D-007) and the AQL traceability queries are well thought out. The M1–M3 acceptance tests are
real integration tests against ArangoDB, cargo and pytest.

The serious problems are at the **trust boundaries**, and they matter now because M5 will run
agent-written code:

1. **Generated code runs with the server's full environment.** That includes the `DEVn_TOKEN`
   Claude tokens that `make dev` exports into the server process. Builds and tests also run with
   no sandbox and with network access. This breaks the "never read/store/forward Claude
   credentials" rule as soon as any untrusted code runs.
2. **`CorsLayer::permissive()` plus no auth.** Any web page the developer visits can drive the
   API, and that includes running arbitrary code through `POST /api/changesets/{k}/test`.
3. **Several API paths bypass the invariants they are meant to protect:**
   - Unvalidated keys are spliced into ArangoDB REST URLs, which allows path traversal to
     immutable `versions` and `releases` documents.
   - The generic edge API accepts *document* collections, so it can insert forged `test_runs`.
   - Rust test results can be spoofed from a test's own stdout.
   - Merge doesn't check that what was reviewed and tested is what gets merged.

Each of these undermines invariants 2, 3 or 6 *while the tests stay green*: the tests only
exercise the front door.

Second tier: cache-key gaps (visibility, renderer version, toolchain and unpinned dependencies are
not in the key), environmental failures cached forever, stale stored closure hashes that can show
false green, and an M4 acceptance gap (release/promote/rollback tools are advertised over MCP but
have no HTTP routes, and the live Claude MCP test silently skips).

Most fixes are small: an env allowlist, one key validator, an edge-collection allowlist, a
review/proposal fingerprint, and junit-style test parsing.

---

## Findings

### 1. Generated code, builds and installs inherit every secret in the server's environment — **critical**

- **Location:** `crates/kodehub-runner/src/exec.rs:46-59` (only `ANTHROPIC_API_KEY` is removed),
  `exec.rs:43` (sandbox profile `(allow default)(deny network*)`), `Makefile:11,33` (`set -a; source
  $(KODEHUB_ENV_FILE)` before starting the server), `testing.rs:242-253,316-328` (tests run with
  `sandbox=false`), `build.rs:194,227` (cargo build and `import kodehub_gen` unsandboxed),
  `pyenv.rs:68-73` (pip/uv unsandboxed).
- **Problem:** D-021 says "No token value is ever written to disk, logged or sent." But the
  server process holds every `DEVn_TOKEN` (and `KODEHUB_ANTHROPIC_API_KEY`,
  `KODEHUB_ARANGO_PASSWORD`, any `CLAUDE_CODE_OAUTH_TOKEN`), and every child process inherits
  them: `cargo build` (third-party `build.rs` and proc-macros), `cargo test`, pytest, run-now and
  `pip install` (`setup.py`). Only run-now is sandboxed, and only against network. Its profile
  still allows reading `~/.claude*`, `~/.ssh` and the env file itself.
- **Scenario:** In M5 a coder agent (or a prompt-injected requirement) proposes a test whose body
  is `print(os.environ)` or `std::env::vars()`. `POST /api/changesets/{k}/test` runs it
  unsandboxed. The output is stored in `test_runs.output` and the changeset's `test_results`,
  pushed over WebSocket, and fed back to agents. So it ends up in transcripts sent to the model
  provider. With network available in tests, it can just be POSTed anywhere. This violates
  CLAUDE.md "Credentials" directly.
- **Recommendation:**
  - (a) In `exec::command`, call `env_clear()` and pass an explicit allowlist (`PATH`, `HOME` or
    a scratch home, `TMPDIR`, `LANG`, `CARGO_HOME`, `RUSTUP_HOME`, `CARGO_TARGET_DIR`,
    `PYTHON*`, `VIRTUAL_ENV`). Add a unit test asserting a `DEV1_TOKEN` set in the parent is
    absent in the child.
  - (b) Don't source the token file into the server at all. Let `scripts/claude-as.sh` source it
    in its own process, and add a `--list-names` mode that prints only variable names for the
    selector. The server then never holds a token.
  - (c) Run tests and the Python import check under the same `sandbox-exec` profile as run-now.
  - (d) Extend the profile to deny `file-read*` outside the render/cache/venv/toolchain dirs, or
    at least deny `~/.claude*`, `~/.ssh` and the env file.
  - (e) Keep network only for the dependency-fetch step. You could `cargo fetch` first, then
    build and test offline with `--offline` under the sandbox.

### 2. Permissive CORS + unauthenticated localhost API = drive-by code execution — **critical**

- **Location:** `crates/kodehub-server/src/lib.rs:50-52` (`CorsLayer::permissive()`),
  `events.rs:38-40` (no Origin check on WebSocket), `routes/changesets.rs:210-234` (`/test` has no
  state or caller check).
- **Problem:** "Single user, local only, no auth" is a deliberate simplification. Permissive CORS
  goes further: it tells every browser origin that it may make credentialed-less JSON requests
  *and read the responses*.
- **Scenario:** The developer visits any web page while `make dev` is running. The page's
  JavaScript does:
  1. `POST http
---

## Full findings list (agent's final summary; file output was truncated)

**Critical**
- **#1 (in the file):** generated code, builds and dependency installs inherit the server's full environment, including the `DEVn_TOKEN` values that `make dev` exports. Builds, tests and installs also run without a sandbox. The fix is an env allowlist in `exec::command`, keeping tokens out of the server process, and sandboxing tests too.
- **#2 (cut off):** `CorsLayer::permissive()` in `crates/kodehub-server/src/lib.rs:52`, together with no auth and no Origin check on the WebSocket, lets any web page drive the localhost API. That includes endpoints that run code. The fix is to allow only the app's own origin and check Origin on `/api/events`.

**High**
- Keys and ids are inserted into ArangoDB REST URLs without validation (`arango.rs:208-250`, `store.rs:130-135`, `store.rs:216-241`). That allows path traversal that skips the immutability checks on versions and releases. Validate keys and percent-encode them.
- The generic edge API accepts document collections (`store.rs:244-270`), so it can write records such as `test_runs` directly. Restrict it to the edge-collection list minus the protected ones.
- Rust test results are parsed from the whole stdout (`testing.rs:259-265`), so a test's own printed output can change its recorded status. Stop parsing at the failure section, or use structured output (junit XML for pytest).
- Running tests as one batch hides missing dependency edges (`testing.rs:225-232`, seed edges are never validated). A pass gets stored under a closure hash that doesn't include everything the test used.
- Merge doesn't check the proposal's base version, so a concurrent change can be silently overwritten. It also drops explicit code edges between untouched symbols (`changesets.rs:603-750`).
- Proposals can still change after a review (`ensure_editable` only blocks terminal states), so unreviewed code can merge. Tie each review to a fingerprint of the proposals it reviewed (invariant 6).

**Medium**
- Stored closure hashes are refreshed outside a transaction, so they can go stale and show a false green. Computing them when needed would be simpler.
- The cache key leaves out visibility, the renderer version, toolchain versions and resolved dependency versions (no `Cargo.lock`, caret version ranges, unpinned pip packages, one shared venv).
- Environmental build failures (network, missing toolchain, timeouts) are cached permanently.
- Every crate's run-now binary is written to the same `target/debug/kodehub_run` path, and only an in-process lock protects the copy into the cache.
- Package and crate specs aren't validated before they reach pip/cargo.
- M4 gap: the `cut_release`, `promote` and `rollback` tools are advertised over MCP but have no HTTP routes. The live Claude MCP test silently passes when it skips, and the MCP stdio test needs a prebuilt CLI binary.
- The MCP `--changeset` scoping is only a default (a caller-supplied changeset wins), and `/` isn't escaped in path parameters.
- A hung test holds the build lock for up to 600 s, and batch retries multiply that.
- One pin conflict anywhere makes every write fail.
- Inferring edges from code bodies can disagree with the hand-written seed edges.
- ArangoDB is published on all network interfaces with the default password.
- Changeset fields are updated by read-modify-write, so concurrent writes can be lost.

**Low**
- The golden test covers content hashes only.
- The M3 test doesn't separate "red via propagation" from "red via a directly verifying test".
- Python run-now passes dicts where a class parameter is expected.
- Merge writes to collections that aren't declared in its transaction.
- Symbol key formats are inconsistent.
- Re-seeding doesn't remove edges deleted from the YAML.
- A bad config file is silently replaced by defaults, and the token-name pattern can be changed over the API.
- The named graph isn't updated when collections are added.
- Duplicate detection reports identical methods on different classes.

**Things done well:** content addressing, the transactional versioning rule backed by a unique index, deterministic rendering, the closure-hash design, the AQL traceability queries, and real integration tests for M1–M3.


