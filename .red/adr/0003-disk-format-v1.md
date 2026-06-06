# ADR 0003 — On-disk format v1.0 stable contract

**Status:** Draft (open for human review)
**Date:** 2026-05-04
**Supersedes:** —
**Superseded by:** —
**Related issues:** [#39](https://github.com/reddb-io/reddb/issues/39)

Operator map: [Storage Profiles](../../docs/deployment/storage-profiles.md)
links this stable byte-format contract to the embedded `single-file` profile
and the first offline embedded-to-operational migration path.

## Context

RedDB is pre-1.0. The CHANGELOG explicitly says "format may change at
any minor version" — and recent history bears it out: this session
alone bumped graph page format v1 → v2 (LabelRegistry refactor),
embedded a `LabelRegistry` blob in the `RBGR` file header, and the
unified entity binary already lives at `STORE_VERSION_V6` after five
prior in-place migrations.

Each format bump today follows an ad-hoc pattern: add a new version
constant, branch the read path on the version byte, leave the write
path on the latest. There is no project-level commitment about
which formats stay stable, how long the engine reads back, or what
tooling operators get when an upgrade fails partway.

Before v1.0 ships, that has to change. Operators planning to run
RedDB across multiple minor releases need to know:

- Which on-disk subsystems are pinned at v1.0 (won't break) versus
  still evolving.
- What happens on first open after a minor-version upgrade — does
  the engine auto-migrate, refuse to open, or open in legacy mode?
- How many versions back the engine reads.
- What recovery looks like when migration fails partway through.
- Whether a primary running v1.0 can replicate to a replica running
  v1.1 (and vice versa) during a rolling upgrade.

This ADR commits answers to all five.

## Decision

### 1. Subsystem stability classification at v1.0

The on-disk surface splits into six independently-versioned
subsystems. Each is tagged either **stable** (frozen at v1.0; future
changes go through a coordinated bump with backward-compat reads) or
**evolving** (no v1.0 stability guarantee; format may change in any
minor release with no migration tooling expected).

| Subsystem               | Path                                               | v1.0 status | Current version       |
|-------------------------|----------------------------------------------------|-------------|------------------------|
| Unified entity binary   | `storage/unified/store/impl_file.rs`               | **stable**  | `STORE_VERSION_V6`     |
| Page layout (pager)     | `storage/engine/page.rs`                           | **stable**  | `PAGE_SIZE = 4096`     |
| WAL records             | `storage/wal/{record,reader}.rs`                   | **stable**  | implicit v1            |
| Graph page format       | `storage/engine/graph_store.rs`                    | **stable**  | v2 (LabelRegistry)     |
| Snapshot manifest       | `storage/wal::SnapshotManifest`                    | **stable**  | implicit v1            |
| Btree node layout       | `storage/btree/node.rs`                            | **evolving** | implicit v1           |

**What "stable" means concretely:**

- The on-disk byte layout will not change in any v1.x minor release
  without a coordinated version bump.
- A new minor release that bumps a stable subsystem's version MUST
  ship a read path for at least the prior version (see compatibility
  window below).
- A new minor release MUST NOT silently rewrite a stable subsystem's
  on-disk data on first open without operator opt-in (see migration
  policy below).

**What "evolving" means:**

- The btree node layout is internal to the secondary-index store and
  not part of the public file format. Operators don't see it directly;
  it's rebuilt from the entity store on demand. Version stamping it
  costs more than it saves — bumping format on a btree change just
  triggers a one-time rebuild on first open, which is the desired
  behaviour.

**What's deliberately out:** Cache files (`page_cache`, hot-entity
cache) are in-memory only and have no on-disk representation; they
don't appear here. Anything under `storage/segments/` was deleted
in this session's pentest pile cleanup and isn't subject to the
contract going forward.

### 2. Migration policy

**Decision:** Lazy auto-migration on read, eager migration on
explicit operator request.

**Concretely:**

- Engine open with format version `N <= current` succeeds. Records
  are read in their stored format; new writes use the current
  format. Pre-existing pages stay on disk in the old format until
  they're rewritten in the normal course of operation.
- Engine open with format version `current + 1` (downgrade attempt)
  fails fast with a clear error: "this database was written by a
  newer engine version (vX) than this binary supports (vY); upgrade
  the binary or restore from backup."
- Engine open with format version `< current - compatibility_window`
  fails with an error pointing at the migration tool: "this database
  predates this binary's read window; run `red migrate <path>` to
  bring it forward, or downgrade to an engine version that still
  reads this format."
- `red migrate <path>` is an explicit CLI command that rewrites
  every page from old format to current. Idempotent. Resumable on
  crash. Produces a `<path>.migration-bak` snapshot before touching
  anything, so a partial migration can be rolled back.

**Why lazy by default:** Eager-rewrite on first open after upgrade
turns minor-version upgrades into hours-long stalls at TB scale.
Lazy keeps RTO unchanged and lets operators schedule the eager
sweep separately.

**Why explicit eager via CLI, not a flag:** A flag like
`--migrate-on-open` invites surprises in CI / auto-restart loops.
A separate command makes the intent loud.

### 3. Compatibility window

**Decision:** Each minor release reads back **N - 1 minor versions**.
v1.5 reads v1.4-format pages but not v1.3. v2.0 ships with explicit
read-path code for v1.x; older versions go through the migration
tool.

**Concretely:**

- v1.0 reads v1.0 only (it's the floor).
- v1.1 reads v1.0 + v1.1.
- v1.2 reads v1.1 + v1.2 (drops v1.0 read path; v1.0 → v1.2 is two
  hops via the migration tool).
- v2.0 reads v1.x as a one-shot upgrade path; new code lives at v2.0.

**Why N-1:** Two-version-back compatibility doubles the read path
surface area and the test matrix without delivering proportional
value — operators upgrade through every minor anyway. One-version-
back is the smallest useful window.

**Caveat for stable-but-rebuilt subsystems:** Btree (evolving,
rebuilt on open) and any future similar systems have no compatibility
window because they have no on-disk persistence to be compatible
with.

### 4. Tooling commitment

**Decision:** A single CLI subcommand, `red migrate`, owns every
operator-facing migration path.

**Surface:**

```
red migrate <path>                  # auto-detect from version, migrate to current
red migrate <path> --check          # report what would be migrated; no writes
red migrate <path> --to <version>   # migrate to a specific version (e.g. for downgrade prep)
red migrate <path> --rollback       # restore from <path>.migration-bak if migration failed
```

**Failure-mode contract:**

- `red migrate <path>` produces `<path>.migration-bak` before the
  first write. Atomic rename of the migrated file at the end.
- A crash mid-migration leaves the original `<path>` intact (because
  the migrated file is written to `<path>.migration-tmp` and only
  rename'd at the end). Partial `<path>.migration-tmp` is safe to
  delete.
- Any failure path includes operator-actionable error text: which
  page it was at, which version it was migrating from/to, the
  rollback command.

**What this commits to:**

- Every minor release that bumps a stable format also lands the
  migrate code path in the same commit. CI gate: `red migrate` round-
  trip test against a v(N-1) golden file must pass before merge.
- The migrate code lives forever even after the format compatibility
  window closes — moving from v1.0 to v2.0 may be two `red migrate`
  invocations (v1.0 → v1.x → v2.0) but never an unrecoverable
  upgrade.

### 5. CDC / replication contract

**Decision:** Replicas during rolling upgrade must run **the same
minor version as the primary, ± 1**. Mismatch beyond that is a
configuration error, not a supported topology.

**Concretely:**

- v1.4 primary streams to v1.4 or v1.5 replicas. v1.6 is rejected.
- v1.4 primary cannot stream to v1.3 replicas (the older replica
  doesn't know how to apply v1.4 records). Streaming connection is
  refused with a clear `version-mismatch` error and an operator
  hint.
- Format-bump replication during upgrade: shut down the replica,
  upgrade it, let it catch up via the new format. No mid-stream
  format flip.

**Why ±1 only:** N-1 reads back; N+1 doesn't because the older
binary doesn't have the future code. Replication is bidirectional
in a sense (a replica sees the primary's written format), so the
window is symmetric.

**What this excludes:** Multi-version replica fleets. If you run a
replica on v1.3 and a primary on v1.5, you get a version-mismatch
error at handshake. Operators upgrade replicas in lockstep with the
primary, one ahead or one behind.

## Consequences

### Positive

- **Operators get a contract.** Today the README warns "pre-1.0,
  format may change". Post-v1.0, that warning is replaced with a
  concrete upgrade story.
- **CI gate prevents silent format breakage.** A change that bumps
  a stable format without landing the migrate path becomes a build
  error.
- **Lazy migration keeps RTO unchanged on upgrade.** Minor releases
  don't stall production for hours.
- **Single tool surface.** `red migrate` is the one place operators
  go for any migration task, not a different command per subsystem.

### Negative

- **More code to maintain per minor release.** Every bump of a
  stable format costs the migrate path plus a one-version-back read
  path. Acceptable; this is the cost of stability promises.
- **Replication ±1 limits operator flexibility.** A multi-version
  replica fleet (sometimes useful for canary upgrades) is rejected.
  Mitigated by the read-replica routing landed in #35 — operators
  can drain a replica out of rotation, upgrade it independently,
  and bring it back without touching the primary.
- **Btree evolving means longer first-open after a btree change.**
  Rebuild cost. Acceptable; btree is a derived index, not source of
  truth.

### Neutral

- **The recent graph format v1 → v2 bump becomes the prototype.**
  In-place lazy migration via `decode_v1` legacy seed mapping;
  next serialize() emits v2; no operator action required. v1.0
  inherits this pattern as the reference shape for future bumps.
- **CHANGELOG conventions update.** Every "Breaking" entry that
  touches a stable format must call out the migrate path explicitly.

## Alternatives considered

1. **Freeze every subsystem at v1.0, no further bumps allowed.**
   Rejected — format evolution is a real need (the LabelRegistry
   refactor is the proof). Freezing forces the project to either
   ship a v2.0 for every storage tweak (operationally hostile) or
   smuggle changes in via flag-gated alternative formats (worse than
   what we have today).
2. **N-2 compatibility window.** Considered — would let operators
   skip a minor. Rejected: doubles the read-path test matrix and
   nobody asked for it. Skip-a-minor upgrades go through `red migrate`.
3. **No compatibility window — every minor reads only its own
   format.** Rejected — minor releases shouldn't require migration.
   That's what major releases are for.
4. **Single monolithic format version (no per-subsystem stamps).**
   Rejected — couples unrelated changes. Bumping the WAL format
   would force a rewrite of every entity page; bumping the graph
   format would invalidate the WAL. Per-subsystem stamps localize
   the cost.
5. **Online format upgrade during a running primary.** Rejected for
   v1.0 — needs a 2-phase cutover protocol that doesn't exist today.
   `red migrate` is offline (engine closed) for now; online upgrade
   is a follow-up issue.

## Open questions

- **What goes into the v1.0 magic header?** Today the unified entity
  binary has `STORE_MAGIC` + version u32. Confirm the same shape
  rolls forward as the v1.0 contract (versus introducing a v1.0
  bookend marker like Postgres's `XLOG_PAGE_MAGIC`). Defer to
  implementation slice.
- **Minor releases between v1.0 and v2.0 — concrete cadence?** Not
  this ADR's job; release engineering decision. Tracked separately.
- **`red migrate --to <version>` for downgrade.** Listed in the
  surface above. Open question is whether downgrade is supported at
  all — usually backups are the operator's downgrade story. Mark as
  "implemented as a stub that errors" for v1.0 unless someone
  produces a concrete need.
- **Auto-migrate at `red server` start with `--migrate-on-open` flag.**
  This ADR rejects it. Reopen if multiple operators ask for it.

---

**Reviewers:** This ADR closes #39 once approved. The
implementation slices that follow are tracked separately:

- `red migrate` CLI subcommand + round-trip test against a v0.x
  golden file.
- CI gate: format-bump PRs must include a passing migrate path.
- README + CHANGELOG conventions update to reference this ADR.

None of those are blockers for v1.0 itself; they land alongside
v1.0's actual format freeze.
