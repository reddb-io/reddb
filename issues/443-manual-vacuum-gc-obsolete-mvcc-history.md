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

- [ ] Manual vacuum identifies reclaimable history versions and tombstones without touching versions visible to active snapshots.
- [ ] Vacuum with an active old snapshot preserves the old snapshot's readable versions.
- [ ] Vacuum after the old snapshot is released reclaims eligible history.
- [ ] Vacuum reports useful counts or metrics for scanned, retained, and reclaimed versions.
- [ ] Tests cover update history, delete tombstones, active snapshots, and post-release cleanup.

## Blocked by

- #438
- #441
