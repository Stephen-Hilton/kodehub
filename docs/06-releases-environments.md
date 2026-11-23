# 06 — Releases and environments

## Environments

| Environment | Purpose | Pilot implementation |
| --- | --- | --- |
| `dev` | Default for run now; uses current working state, before any release | Local subprocesses, stubs allowed |
| `test` | Where all automated tests run | Local subprocesses |
| `qa` | Pre-prod; mirrors prod | Simulated: separate local process + config |
| `prod` | Live | Simulated: separate local process + config |

Each environment has an **environment agent** that deploys and gates promotion. Promotion into
`prod` always requires human sign-off.

## Releases

- Cut often; several per day is normal.
- A release is an **immutable manifest**: every symbol in scope mapped to an exact version hash,
  plus the root closure hashes and build targets. Creating one is cheap.
- Code links to releases via `included_in` edges. Code **never** links to environments directly.
- **Diff** between releases = set difference of version hashes → exactly which symbols changed.
- **Bisect** across releases to find which change introduced a failure.

## Promotion and "current"

- Promotion adds a `deployed_to` edge from the same release to the next environment. Nothing is
  copied or migrated.
- Each environment has **at most one** `currently_running` edge to a release. On deploy, the old
  edge becomes `previously_ran` with `{ from, until }`, in one transaction.
- "Current" is therefore per environment, not a property of a release; a release can be current
  in `qa` while `prod` runs an older one.
- **Rollback** = point `currently_running` back to the most recent `previously_ran` release.

## Gates (pilot defaults)

- `dev → test`: release builds for all targets.
- `test → qa`: all tests pass; coverage shows no failing requirements; no orphans.
- `qa → prod`: QA environment agent smoke run passes; human approves.
