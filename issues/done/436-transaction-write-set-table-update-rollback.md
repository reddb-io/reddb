# Transaction write set for table UPDATE rollback [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/436

Labels: enhancement

GitHub issue number: #436

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Introduce table-row transaction write-set behavior for UPDATE so explicit transactions can read their own pending updates, hide uncommitted updates from other transactions, and roll back update work without mutating committed state.

## Acceptance criteria

- [ ] UPDATE inside an explicit transaction is staged in transaction-local state until commit.
- [ ] The writing transaction reads its own pending UPDATE.
- [ ] Other transactions do not see the pending UPDATE before commit.
- [ ] ROLLBACK after UPDATE leaves the previously committed value visible.
- [ ] Autocommit and explicit-transaction update paths share the same MVCC versioning semantics after commit.

## Blocked by

- #435
