# Manual VACUUM/GC for obsolete MVCC history [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/443

Labels: enhancement

GitHub issue number: #443

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Add a manual VACUUM/GC path for obsolete MVCC history that reclaims table-row history and tombstones only when they are older than every active snapshot and any configured retention floor.

## Acceptance criteria

- [x] Manual vacuum identifies reclaimable history versions and tombstones without touching versions visible to active snapshots.
- [x] Vacuum with an active old snapshot preserves the old snapshot's readable versions.
- [x] Vacuum after the old snapshot is released reclaims eligible history.
- [x] Vacuum reports useful counts or metrics for scanned, retained, and reclaimed versions.
- [x] Tests cover update history, delete tombstones, active snapshots, and post-release cleanup.

## Blocked by

- #438
- #441

## Progress

- 2026-05-13: Implemented manual MVCC history reclamation through the
  existing `VACUUM [table]` command.

  What changed:
  - VACUUM now computes a safe cutoff from the next xid, oldest active
    transaction xid, oldest pinned xid, and optional
    `runtime.mvcc.vacuum_retention_xids`.
  - Table-row versions with `xmax < cutoff` are physically reclaimed;
    versions visible to active or pinned snapshots are retained.
  - Delete tombstones are distinguished from update-history versions in
    the reported counters.
  - Runtime secondary indexes are rebuilt for a table when VACUUM
    reclaims rows, preventing stale index entries from hiding the live
    replacement row.
  - The aborted-xid set is pruned below the same cutoff.
  - VACUUM messages now include scanned, retained, and reclaimed counts.

  Tests:
  - `tests/e2e_mvcc_vacuum.rs` covers update history and delete
    tombstones with an active old snapshot, then post-release cleanup.

  Verification:
  - `cargo check -q` passed.
  - `cargo test -q --test e2e_mvcc_vacuum -- --test-threads=1` → 2 passed.
  - `cargo test -q --test e2e_mvcc_index_recheck -- --test-threads=1` → 2 passed.
  - `cargo test -q --test e2e_mvcc_delete_tombstones -- --test-threads=1` → 3 passed.
  - `git diff --check` passed.
