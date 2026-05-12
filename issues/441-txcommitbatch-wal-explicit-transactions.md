# Atomic TxCommitBatch WAL for explicit multi-statement transactions [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/441

Labels: enhancement

GitHub issue number: #441

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#432

## What to build

Extend atomic TxCommitBatch WAL semantics to explicit multi-statement transactions so a committed transaction with multiple table-row INSERT, UPDATE, and DELETE operations recovers all-or-nothing after restart.

## Acceptance criteria

- [ ] COMMIT for an explicit transaction writes one commit batch that represents all staged table mutations.
- [ ] Crash before the complete batch leaves the transaction absent after restart.
- [ ] Crash after the complete batch recovers every mutation in the transaction after restart.
- [ ] No partial transaction state is visible after recovery.
- [ ] Recovery replay is idempotent across repeated restarts.

## Blocked by

- #439
- #440
