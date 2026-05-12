# Savepoint rollback restores UPDATE pre-image [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/437

Labels: enhancement

GitHub issue number: #437

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Complete savepoint semantics for table-row UPDATE by making ROLLBACK TO SAVEPOINT discard update work after the savepoint and restore the transaction-local pre-image visible before that savepoint.

## Acceptance criteria

- [ ] UPDATE after SAVEPOINT is undone by ROLLBACK TO SAVEPOINT.
- [ ] UPDATE before SAVEPOINT remains visible after rolling back later savepoint work.
- [ ] Multiple nested savepoints restore the correct transaction-local row version.
- [ ] The existing ignored savepoint UPDATE reversal regression is enabled or replaced by an active equivalent test.
- [ ] RELEASE SAVEPOINT preserves update work as documented.

## Blocked by

- #436
