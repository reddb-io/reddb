# MVCC index recheck and historical fallback [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/442

Labels: enhancement

GitHub issue number: #442

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Make indexed table reads MVCC-correct by rechecking logical identities through the MVCC resolver and falling back when the current index cannot prove a historical snapshot answer.

## Acceptance criteria

- [ ] Indexed lookups return only versions visible to the active snapshot.
- [ ] Full scans and indexed scans agree for current snapshots and historical snapshots.
- [ ] Updating an indexed column does not make old snapshots lose the prior value.
- [ ] Deleting an indexed row does not make old snapshots lose the prior row.
- [ ] Tests cover stale-current-index cases and compare indexed vs non-indexed query results.

## Blocked by

- #435
- #439
