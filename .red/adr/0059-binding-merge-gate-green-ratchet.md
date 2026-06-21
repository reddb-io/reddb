# ADR 0059 — Binding merge gate + green ratchet on `main`

Status: accepted
Date: 2026-06-21

Implements issue #975 (PRD #964 — Test & Verification Hardening). Supersedes the
prior "admin-merge bypasses CI" posture. (Issue #975's body referenced "ADR 0048"
for the admin-merge posture; that number is wrong — ADR 0048 is *Desktop
self-sufficiency via a bundled `red` sidecar*. This ADR is the actual record of
the gating decision.)

## Decision

`main` is a **protected branch** with **required status checks** and
**`enforce_admins` enabled**. Concretely:

- A PR may merge to `main` **only** when every required CI check is green.
- `strict` is on (branch must be up to date with `main` before merge), so a red
  `main` blocks the whole queue until it is green again — the **ratchet**.
- `enforce_admins` is on, so the **AFK admin-merge path** (`gh pr merge --admin`)
  no longer bypasses the gate: AFK-authored changes — including
  storage/replication-touching ones — are blocked when their scope is red, the
  same as any human PR.

### Required check contexts (the gate)

The reddb CI jobs that run on every PR are required:

`gate`, `Quality (fmt, check, clippy)`, `Lint (no untyped serialization)`,
`Version integrity`, `Contract Matrix Gate`, `Docs Match Contract Matrix`,
`Helm Chart`, `AFK Validation Sidecar`, `RQL Conformance (sqllogictest)`,
`Drivers / Python (cargo check)`, `Feature Matrix (all-features | backend-d1 |
backend-s3 | backend-turso | no-default | otel)`, `Test Suite`,
`Driver Param Conformance`, `Chaos & Drill Suite`, `Fuzz Parsers`,
`Container Stack`, `Publish Dry-Run (crates.io)`, `cargo package dry-run`,
`Windows (build + unit tests)`, `macOS (build + unit tests)`.

Deliberately **not** required: `Chaos Suite (Floci S3 backend)` (runs only under
`workflow_dispatch` with `full_ci`, so it never reports on a PR and would pin the
branch in a permanent-pending state); the changeset-automation jobs
(`Auto-approve patch/minor bumps`, `Verify update compiles`); and third-party
review bots (`CodeRabbit`).

## Why

The merge gate is the keystone of PRD #964: without it, AFK pushes land on `main`
without running the suite (the "main CI gap"), so `main` accumulates rot that
every later PR inherits. Making the suite a hard, admin-respecting gate stops new
rot at the door and makes a red `main` everyone's problem, not a silent one.

## Consequence (read before relying on this)

- **All merges freeze while `main` / the affected scope is red — including
  hotfixes and the AFK drain.** At adoption `main` was red (broken tests plus the
  finding in #974 that the full integration/e2e lane cannot compile + run to
  completion within CI budget), so enabling the gate produces an immediate total
  freeze by design — the intended forcing function to push suite-hardening to the
  top.
- **Un-freezing a red `main` requires temporarily lifting protection** to land
  each hardening fix, then re-enabling — because the fixes themselves cannot merge
  through the gate they are meant to satisfy.
- **The integration/e2e lane is the explicit ratchet target.** Per #974 it is too
  heavy to gate today; its required-check membership is added once the lane is
  sharded/fitted and `main` is greenable.
- **Enforcement is split across two repos.** The GitHub-side half (branch
  protection + `enforce_admins`) is set on `reddb-io/reddb`. The AFK-bundle-side
  half — teaching `/afk`'s admin-merge to pre-check the gate and route a red scope
  to `ready-for-human` instead of force-landing — lives in `reddb-io/red-skills`
  and is a follow-up.
