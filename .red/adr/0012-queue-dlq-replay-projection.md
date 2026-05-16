# ADR 0012: Queue DLQ Replay And Read-Only Queue Projection

Status: accepted
Date: 2026-05-10
Issue: #282

## Context

The landing copy previously mentioned `QUEUE MOVE FROM <src> TO <dst> [WHERE ...]`
and `SELECT ... FROM QUEUE <name>`, but the engine did not implement either
form. The UX is still useful: operators need to inspect DLQs with filters and
replay selected messages back to a live queue without writing ad hoc scripts.

Both verbs touch queue transactional semantics, consumer-group state, and the
boundary between queue commands and normal SQL planning.

## Decision

RedDB will add a queue-specific replay command:

```sql
QUEUE MOVE FROM failed_jobs TO jobs
  WHERE attempts >= 3 AND last_error LIKE 'timeout%'
  LIMIT 100;
```

`QUEUE MOVE` is a bounded, all-or-nothing queue transaction. The command takes a
snapshot of eligible source messages at command start, evaluates `WHERE` against
that snapshot, selects at most `LIMIT` rows, and commits the source removal plus
destination append in one runtime transaction. If any selected message cannot be
appended to the destination, no selected source message is removed.

`LIMIT` is required for `QUEUE MOVE` when a `WHERE` clause is present. Without a
`WHERE` clause, the default limit is one message. This keeps replay predictable
and prevents one command from becoming an unbounded queue migration.

RedDB will also add a read-only queue projection:

```sql
SELECT id, payload, attempts, last_error, enqueued_at
FROM QUEUE failed_jobs
WHERE attempts >= 3
LIMIT 50;
```

`SELECT FROM QUEUE` is a queue-only read path, not a full table-planner rewrite.
It returns a `UnifiedResult` with the queue projection schema below and never
consumes, leases, ACKs, NACKs, or mutates consumer-group state.

Projection columns:

| Column | Meaning |
|---|---|
| `id` | Stable queue message id / sequence id |
| `payload` | Message payload as JSON/value text |
| `priority` | Queue priority |
| `attempts` | Delivery attempts counted by the queue runtime |
| `last_error` | Last NACK/error reason when available |
| `enqueued_at` | Enqueue timestamp |
| `available_at` | Next availability timestamp after delay/retry |
| `dlq` | Whether the message is in a configured dead-letter queue |
| `tenant` | Tenant scope when present |

`QUEUE PEEK` remains the low-level operational command for quick inspection, but
it does not cover filtered DLQ triage or column projection. It is not enough to
defer `SELECT FROM QUEUE`.

## Consequences

- Queue replay stays in the queue runtime. It does not pretend queues are normal
  tables and does not expose queue messages to arbitrary SQL mutation.
- The implementation can share expression evaluation for simple `WHERE`
  predicates, but the executor is queue-specific.
- Consumer groups are unaffected by read-only projection. `QUEUE MOVE` moves
  physical messages and drops any source queue lease state for selected messages.
- DLQ replay is an explicit operator action and should be audited as
  `queue/move` with source, destination, selected count, and committed count.

## Follow-Up

Open an implementation issue covering parser, AST, queue executor, docs, and
tests for `QUEUE MOVE` and `SELECT FROM QUEUE`.
