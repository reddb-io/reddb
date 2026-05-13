# MVCC DELETE tombstones preserve old snapshots [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/438

Labels: enhancement

GitHub issue number: #438

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Make table-row DELETE a tombstone-producing MVCC operation. Current and future snapshots should see the logical row as deleted after commit, while snapshots that predate the delete can still resolve the prior committed version.

## Acceptance criteria

- [ ] DELETE creates a tombstone version instead of immediately physically removing the logical row.
- [ ] A transaction opened before the DELETE continues to read the old row after the delete commits.
- [ ] A transaction opened after the DELETE does not see the row.
- [ ] DELETE inside an explicit transaction remains invisible to other transactions until commit.
- [ ] ROLLBACK of a staged DELETE leaves the row visible.

## Blocked by

- #435
- #436
