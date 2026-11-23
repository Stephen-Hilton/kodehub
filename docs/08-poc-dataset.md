# 08 — Proof-of-concept dataset and demo

Seed lives in `examples/auth-poc/` as YAML files (one per collection, plus `edges.yaml`). Write
the actual code bodies yourself; keep them small and idiomatic.

## Content

**Mission (program):** "Provide secure, simple authentication that never leaks information about
why a login failed."

**C4**
- Context: Authentication
  - Container: auth-service
    - Component: credential-verification
    - Component: password-hashing

**Symbols**

| Symbol | Language | Component | Notes |
| --- | --- | --- | --- |
| `hash_password(password) -> String` | Rust | password-hashing | Argon2id via the `argon2` crate |
| `verify_password(password, hash) -> bool` | Rust | password-hashing | |
| `UserStore` (class/struct) with `find_user(username) -> Option<User>` | Rust | credential-verification | In-memory store for the pilot |
| `verify_credentials(store, username, password) -> AuthResult` | Rust | credential-verification | Calls `find_user`, `verify_password` |
| `login(username, password) -> dict` | Python | credential-verification | Pilot: pure Python calling a Python port; M8: `binds_to` the Rust function |
| Tests | Rust and Python | | Written from the requirements |

**Requirement group:** Secure authentication (`all_of`), `traces_to` the mission, satisfied by
the Authentication context.

- **R1 — Generic failure.** Statement: failed logins return one generic error. Constraints: the
  same error for unknown user and wrong password. Examples: unknown user → `invalid_credentials`;
  wrong password → `invalid_credentials`. Edge cases: empty username → `invalid_credentials`.
- **R2 — Hashed storage.** Statement: passwords are stored only as Argon2id hashes. Examples:
  hashing the same password twice gives different hashes that both verify.

**Decision:** "Do not reveal which credential failed" — rationale: prevents username enumeration.
Linked by `justified_by` from `verify_credentials`.

**Planted fixtures for tests:** a deliberate duplicate of `verify_password` under another display
name (for the duplicates query), removable after M3.

## Demo scenario (used by M5 and M6 acceptance)

1. Start with everything green.
2. Human adds an edge case to R1: "A locked account must also return `invalid_credentials`."
3. R1 and everything beneath it turns amber; the orchestrator opens a changeset and shows ghost
   nodes (e.g. a `locked` field on `User`, an updated `verify_credentials`).
4. Human approves the plan.
5. Tester adds a test for the locked case from the requirement; coder updates the code; reviewer
   reviews blind and raises at least one finding (e.g. timing difference between paths).
6. Coder and reviewer resolve it; a Decision node records the outcome.
7. Tests pass; changeset merges; map returns to green.
8. Human selects `verify_credentials`, clicks run now with a locked user, and sees
   `invalid_credentials` instantly (cache hit).
9. Cut a release and promote it to `test`.
