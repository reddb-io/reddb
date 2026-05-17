# ADR 0020: QueueLifecycle Module Contract

Status: proposed
Date: 2026-05-16
Issue: TBD

## Context

Queue delivery and retirement logic today is split between
`runtime/impl_queue.rs` (2122 LOC) and `runtime/queue_delivery.rs` (470 LOC,
`pub(super)` only). The split is structural but not encapsulated:
`queue_delivery` reaches back into `impl_queue` for
`queue_message_lock_handle`, `queue_message_pending_any`, and
`queue_message_view_by_id`. Callers repeat the same decision sequence —
is-locked? → is-pending? → work-vs-fanout? → retry-or-DLQ? — at every
ACK/NACK/POP site.

`.red/CONTEXT.md` already names the sub-steps (**Queue delivery**, **Queue
retirement**, **Pending delivery**) but no Module owns them. This ADR records
the contract for a deep `QueueLifecycle` Module that owns the full state
machine. Decisions below are interlocking — the replica path only works
because the primary owns decisions in the caller's transaction; the
`QueueStore` adapter only earns its keep because the replica is a second real
adapter.

## Decision

### Scope and shape

- `QueueLifecycle` lives at `runtime/queue_lifecycle.rs`. It owns the full
  state machine: pick → lock → ack/nack/drop/DLQ. Enqueue and storage stay
  outside (enqueue is co-located with mutation/events; storage is a lower
  layer).
- One Module covers both `WORK` and `FANOUT`; mode is data, not a type
  parameter. The transitions are structurally identical — only the fan-out
  cardinality of pending deliveries differs.
- Retry/DLQ policy is owned directly, read from queue catalog metadata
  (`CREATE QUEUE ... WITH DLQ ...`). No injected `RetirementPolicy` adapter —
  it would have exactly one real implementation, violating "two adapters = real
  seam."
- Consumer group create/delete remains catalog DDL outside the Module.
  `QueueLifecycle` reads the group list to fan out pending rows.

### State machine

- Lock expiry is handled by **lazy reclaim** on the next `deliver()` call. No
  background sweeper. Concentrates the lifecycle in one Module with no timer
  surface to test.
- Lock granularity is per-message (today's `queue_message_lock_handle`
  behavior). Per-(queue, group) would serialize the group and kill `WORK`
  throughput.
- Ordering inside the Module is at-least-once with no order. Strict FIFO would
  require head-of-line blocking per group; users wanting FIFO use a single
  consumer.
- `nack()` returns `()`. The Module internally chooses `Requeued |
  MovedToDlq | Dropped` and emits a `RetirementOutcome` event for
  observability. Exposing the enum would re-duplicate the policy switch at
  every caller — the friction this Module exists to remove.
- DLQ is a regular queue (per `internal: true` flag in ADR-0012). DLQ writes
  recurse through the same enqueue path, so replay/projection works out of
  the box.
- No message TTL in v1. Time-based eviction adds either a background scan or
  a lazy check on every deliver; no caller asks for it today.
- No long-poll on `deliver()`. Returns immediately empty. Long-poll lands later
  as a transport optimization, not a Module concern.
- No lock extension RPC in v1. Lock deadline is fixed at delivery time. If
  jobs are long, configure a longer initial deadline on the queue.
- No queue-depth cap on enqueue. Only the per-group in-flight cap applies.
  Depth caps need an eviction policy and constrain legitimate burst producers.
  Hard quota belongs at tenant-quota layer.

### Atomicity and replica determinism

- `QueueLifecycle` operations participate in the **caller's transaction**
  (Statement frame). Splitting transactions reopens the dual-write window
  ADR-0015 closes for events. ACK/NACK semantics depend on whether the
  consumer's downstream work committed.
- Crash recovery honors the lock deadline persisted in WAL. Pending deliveries
  are not flushed on restart — that would thundering-herd duplicate messages
  for clients still legitimately processing across the restart.
- **Replicas replay outcomes, never re-decide.** Decisions (which message to
  deliver, which to DLQ, retry counts) are non-deterministic across nodes.
  Primary `QueueLifecycle` decides; WAL carries the outcome; replica's
  Logical change applier dumbly replays. Matches the existing replication
  contract.

### Testing seam

- `QueueLifecycle` depends on a narrow `QueueStore` trait (read pending,
  append WAL, update attempt counter, enqueue to DLQ target). Not `&Engine`.
  The trait has two real adapters in sight — the in-engine path and the
  replica replay path via the Logical change applier — so it passes "two
  adapters = real seam."

### Auth

- `QueueLifecycle` trusts the caller for authorization. Every entry point is
  inside a Statement frame which already owns `EffectiveScope` checks.
  Re-checking inside the Module duplicates auth at every call site and
  couples the Module to the auth subsystem.

### Observability

- DLQ promotion emits an **OperatorEvent** (audit + paging channel). Data
  leaving the normal flow is forensic.
- Lock reclaim emits **developer signal** via `tracing` only. High-volume
  routine event.
- Prometheus counters per ADR-0017: `queue_delivered_total`,
  `queue_acked_total`, `queue_nacked_total{outcome=dlq|retry|drop}`,
  `queue_pending_gauge`. Labels `{queue, group, mode}`; cardinality bounded
  by catalog.

### Wire contract and migration

- Server-issued opaque `delivery_id` (base32). Identifies a `Pending
  delivery` row. Any consumer in the group can ACK/NACK with it
  (`delivery_id` is the capability — no session affinity).
- Existing ACK/NACK wire shapes preserved. `delivery_id` added as an
  optional response field; legacy `(queue, message_id, consumer)` tuple
  path still works one minor release. Avoids forcing redwire + gRPC +
  Postgres-wire driver bumps simultaneously.
- Atomic switch — `QueueLifecycle` replaces `impl_queue` + `queue_delivery`
  in one PR. The two paths share state (pending deliveries, attempt
  counters); flag-gating risks divergent decisions on the same message.

### Introspection

- New `red.queues` virtual table registered in `runtime/red_schema.rs`,
  exposing queue-specific columns: `mode`, `depth`, `total_pending`,
  `oldest_pending_age`, `dlq_target`. `SHOW QUEUES` desugar repoints from
  filtered `red.collections` to `red.queues`.
- Per-row pending drill-down lives in a separate `red.queue_pending`
  virtual table (cold scan, different consistency tier than the hot
  catalog-snapshot fields).
- Replay/projection is a separate read-only Module reading queue WAL
  records; not bundled into `QueueLifecycle`.

## Consequences

- The state machine concentrates in one Module with one test suite. Future
  bug fixes to transitions don't have to be applied at three call sites.
- Replicas stay deterministic; primary failover preserves decisions because
  outcomes — not inputs — are replicated.
- `QueueStore` trait enables unit-level testing without booting the engine.
  Currently every queue test is an integration test.
- Wire compatibility window: drivers have one minor release to adopt
  `delivery_id`. Beyond that, tuple ACKs hard-deprecated.
- No background machinery — no sweeper, no TTL reaper, no lock extender. The
  Module is sync, transactional, and dependency-free. If long-poll, TTL, or
  lock extension demand emerges, they layer on top.

## Alternatives considered

- **Internal transaction for retirement.** Rejected: reopens the ADR-0015
  dual-write window.
- **`&Engine` god-arg.** Rejected: makes every test boot the world and
  blocks the Module from being independently testable.
- **Background sweeper for lock expiry.** Rejected: second owner of
  transitions, timer surface to test, no functional gain over lazy reclaim.
- **Structured `delivery_id` (queue:group:lsn:attempt).** Rejected: clients
  would parse it and constrain future shape changes.
- **Long-poll inside the Module.** Rejected: adds connection-holding +
  notify/wake surface; belongs at transport layer.
- **Single `red.collections` extension with queue columns.** Rejected:
  violates the `SHOW QUEUES` faithful-to-type rule and pollutes the table
  with NULL columns for non-queues.

## References

- ADR 0010: serialization-boundary-discipline
- ADR 0012: queue-dlq-replay-projection
- ADR 0014: mvcc-history-store-and-transaction-recovery
- ADR 0015: events-dual-write-window
- ADR 0017: prometheus-grafana-adapters-for-metrics
- `.red/CONTEXT.md` → `QueueLifecycle`, `Queue delivery`, `Queue retirement`,
  `Pending delivery`, `WORK queue`, `FANOUT queue`
