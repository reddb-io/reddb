# Queue MOVE and SELECT FROM QUEUE implementation [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/331

Labels: enhancement

GitHub issue number: #331

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with
tests/checks, commit all changes, and move this file to `issues/done/` when
complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#282

## Design

ADR: `docs/adr/0012-queue-dlq-replay-projection.md`

## What to build

Implement the accepted DLQ replay and read-only queue projection design.

- Parser/AST support for `QUEUE MOVE FROM <src> TO <dst> [WHERE <expr>] [LIMIT <n>]`.
- Parser/AST support for `SELECT <projection> FROM QUEUE <name> [WHERE <expr>] [LIMIT <n>]`.
- Queue runtime execution where `QUEUE MOVE` snapshots eligible source messages, evaluates the predicate, selects a bounded batch, and atomically removes from source plus appends to destination.
- `SELECT FROM QUEUE` returns the ADR projection schema without consuming, leasing, ACKing, NACKing, or mutating consumer-group state.
- Audit event for `queue/move` with source, destination, selected count, and committed count.
- Docs updates for queue DLQ replay and inspection.

## Acceptance criteria

- [x] `QUEUE MOVE FROM failed_jobs TO jobs WHERE attempts >= 3 LIMIT 100` parses and executes.
- [x] `QUEUE MOVE` requires an explicit `LIMIT` when `WHERE` is present; without `WHERE`, default limit is 1.
- [x] Move is all-or-nothing for the selected batch: destination append failure leaves source unchanged.
- [x] `SELECT id, payload, attempts, last_error, enqueued_at FROM QUEUE failed_jobs WHERE attempts >= 3 LIMIT 50` returns read-only projection rows.
- [x] `SELECT FROM QUEUE` does not consume, lease, ACK, NACK, or modify consumer-group state.
- [x] `QUEUE PEEK` remains backward-compatible.
- [x] Tests cover parser, runtime move, read-only projection, and failure atomicity.

## Blocked by

None - can start immediately.
