# Atomic TxCommitBatch WAL for autocommit mutations [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/440

Labels: enhancement

GitHub issue number: #440

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Persist autocommit INSERT, UPDATE, and DELETE as atomic transaction commit batches in the WAL so crash recovery can replay or discard each autocommit mutation as one complete unit.

## Acceptance criteria

- [ ] Autocommit table mutations write a complete commit-batch WAL record before acknowledgment.
- [ ] Crash before a complete commit batch leaves the mutation absent after restart.
- [ ] Crash after a complete commit batch recovers the mutation after restart.
- [ ] Replaying the same commit batch is idempotent for current store, history store, indexes, and tombstones.
- [ ] Tests cover INSERT, UPDATE, and DELETE autocommit recovery.

## Blocked by

- #435
- #438
