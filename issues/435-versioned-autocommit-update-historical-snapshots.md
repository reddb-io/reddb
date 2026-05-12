# Versioned autocommit UPDATE with historical snapshot reads [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/435

Labels: enhancement

GitHub issue number: #435

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Make autocommit table-row UPDATE versioned under MVCC: an update creates a new physical version for the same logical row, preserves the previous committed version in history, and lets old snapshots resolve the old value while new snapshots see the updated value.

## Acceptance criteria

- [ ] Autocommit UPDATE preserves a prior committed version for active historical snapshots.
- [ ] A transaction opened before the update continues to read the old value after the update commits.
- [ ] A transaction opened after the update reads the new value.
- [ ] The visible-row resolver is the authority for current vs historical version selection in this path.
- [ ] Regression tests demonstrate no snapshot read skew for updated rows.

## Blocked by

- #434
