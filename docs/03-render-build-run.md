# 03 — Render, build, test and run

Toolchains need files. The renderer (the "pre-compiler") creates them on demand in a temp
directory, and throws them away afterwards. Files are never the source of truth.

## Render

Input: a set of root symbols plus a build target. Steps:

1. Compute the dependency closure by traversing `calls`, `depends_on`, `member_of`, `tested_by`
   (tests only for test builds), honouring `pinned_version`.
2. Resolve each symbol to its current (or pinned) version.
3. Topologically sort by dependencies.
4. Emit a project:
   - **Rust:** a temporary Cargo crate. All items go into `src/lib.rs` in topological order
     (pilot simplification). External crates come from an `attributes.crates` list on versions
     and are merged into `Cargo.toml` with pinned versions.
   - **Python:** a temporary package with all items in `__init__.py` in topological order.
     External packages come from `attributes.packages`.
5. Namespaces, if present for the build, map to `pub mod` blocks (Rust) or sub-modules (Python)
   that re-export the exposed symbols under their `public_name`.
6. Write a `render-manifest.json` listing every version hash and the root `closure_hash`.

Rendering must be deterministic: same closure → byte-identical output.

## Build cache

- Key: `closure_hash` + `build_target`.
- Location: `~/.kode/cache/<closure_hash>/<build_target>/`, recorded in `artifacts`.
- Before any build or run: look up the key. Hit → use it. Miss → render, compile, store.
- Every merged changeset ends with compile and test, so the cache is normally warm and "run now"
  is instant, including for Rust.
- Use a shared `CARGO_TARGET_DIR` under `~/.kode/cargo-target/` to speed up cold Rust builds.

## Test

- Rust tests render into a `#[cfg(test)] mod kodehub_tests` and run with `cargo test`.
- Python tests render into `tests/test_kodehub.py` and run with `pytest -q --json-report` (or parse
  output if the plugin is unavailable).
- Each test result is stored in `test_runs` against the test version and the closure hash it ran
  under. A cached passing result for the same closure hash does not need re-running.

## Run now

1. User selects a function symbol and an environment (default `dev`).
2. The UI generates a parameter form from the version's `signature`. Pilot-supported types:
   integers, floats, bool, strings, `Option`/optional, lists and maps of these, and structs/classes
   made only of these. Anything else disables run now with an explanation.
3. The runner looks up the artifact for the closure hash (compiling only on a miss).
4. It generates a tiny harness:
   - Rust: a bin target that reads JSON args from stdin with `serde_json`, calls the function and
     prints the JSON result.
   - Python: imports the rendered package, reads JSON args from stdin, prints the JSON result.
5. Execute in a temp directory with a 10-second timeout and no network access where the OS allows.
6. Stream stdout/stderr to the UI; store the run in `runs`.
7. Offer **"Save as example"**: attach `{ input, expected: actual_output }` to a requirement the
   function serves (chosen from those reachable upward). The tester agent turns it into a test.

## Side effects

Functions that touch external systems run against the selected environment's configuration.
In the pilot, `dev` provides stand-ins: the coder agent may generate stub symbols tagged `stub`
that replace external calls when rendering for `dev`.
