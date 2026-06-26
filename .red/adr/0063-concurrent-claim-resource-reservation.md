# ADR 0063: Concurrent claim for transactional resource reservation

Status: proposed
Date: 2026-06-26

## Context

RedDB already exposes tables, KV, queues, events, and transactions inside one
engine. The resource-reservation problem that motivated this ADR is the classic
oversell race: multiple callers try to reserve the same finite capacity, such as
stock units, seats, slots, or quotas, while the application also needs
idempotency and downstream workflow delivery.

The tempting external architecture is to coordinate reservation in Redis or a
queue, then write the source of truth in RedDB. That repeats the dual-write
window ADR 0015 closed for `WITH EVENTS`: two systems must agree on one logical
business action without sharing one commit boundary.

RedDB needs a native claim primitive that lets competing transactions make
progress on different eligible records, while keeping reservation state,
idempotency state, and event/queue side effects inside the RedDB authority.

## Decision

**Concurrent claim is a state-changing mutation.** The public shape is an
`UPDATE`-like operation with claim semantics, not a lock-only
`SELECT ... FOR UPDATE` surface. The operation chooses eligible records, applies
the requested state transition, and may return the claimed identities through
`RETURNING`. This keeps the user-facing operation aligned with the business act:
available capacity becomes reserved capacity.

**Claim candidates require deterministic, index-backed ordering.** A claim must
specify an explicit order backed by a compatible index. RedDB chooses from that
ordered candidate set, but skips records that are already held by another
in-flight claim. Without an explicit order, or with an order the planner cannot
serve through an index, the statement is rejected rather than relying on
physical layout, incidental scan order, or a broad write-path sort.

The compatible index should cover the principal equality filters as a prefix and
then the claim order columns. A typical reservation shape is
`product_id, location_id, state, expires_at, rid`: narrow to one stock pool,
then claim in a deterministic order.

If the user-provided order is not total, RedDB appends the model's stable claim
identity as an implicit tie-breaker. Ties must never fall back to physical scan
order.

**Claim locks are transaction-scoped and claim-scoped.** When a transaction
claims a candidate, RedDB takes a `Claim lock` on the model-owned `Claim
identity`. Other claimers skip that candidate until the transaction commits or
rolls back. Ordinary DML keeps the normal MVCC conflict policy; a claim lock is
not a broad write lock for every update path.

**Claim identity is model-owned.** Tables and documents use stable logical
identity. KV uses key identity. Queues participate through `QueueLifecycle`
delivery identity, not by exposing a second raw claim path over queue storage.
This preserves ADR 0020's single authority for queue delivery, ACK/NACK, retry,
DLQ, and replica replay.

**Partial claim is the default; exact claim is explicit.** `CLAIM LIMIT n`
returns as many immediately claimable records as it can. `CLAIM EXACT n` commits
only when the requested cardinality is fully satisfied by immediately claimable
records. If exact cardinality cannot be satisfied, the statement is a normal
`Claim miss`: it applies no writes and reports zero affected rows rather than a
storage error.

**No wait mode in the first contract.** A claim skips in-flight candidates and
does not wait for them. Waiting inside explicit transactions would hold
transaction-local state while another writer determines progress, repeating the
same caution that keeps `QUEUE READ ... WAIT` autocommit-only. A future wait
mode needs its own decision.

**The isolation contract is snapshot plus candidate claim locks.** RedDB does
not introduce predicate locks, gap locks, SSI, or phantom prevention as part of
the first claim contract. A range predicate claims from the transaction snapshot
and skips currently claim-locked candidates. Records inserted later by other
transactions may be claimed by those transactions if they are otherwise eligible.

**Explicit transactions are supported.** A claim lock lives until
`COMMIT`/`ROLLBACK`, so an application can claim capacity, write the order or
reservation record, store a reservation idempotency key, and emit events or queue
work in one transaction boundary.

**Claim authorization composes read and update policy.** Candidate selection
obeys the same read visibility rules as the underlying model, including tenant
scope and RLS where applicable. The state transition then obeys the update policy
for the changed fields. A caller must not claim a record it cannot see, and must
not use claim to bypass update restrictions.

**Idempotency is a recipe requirement, not built into the primitive.** RedDB
does not add an `IDEMPOTENCY KEY` clause to claim itself. The canonical
reservation recipe stores an application-defined idempotency key in a table or
KV collection inside the same transaction so retried requests observe the prior
outcome rather than creating a duplicate reservation.

**Claim authority follows write ownership.** A claim is a non-deterministic
write decision and must be made only by the write primary or range owner.
Replicas route or reject according to normal write rules and replay outcomes;
they never choose claim winners locally.

**The first cluster contract is owner-local.** Cross-owner exactness is outside
the first claim contract. In sharded clusters, the first productive claim path is
restricted to one write owner or range owner. Claims that require multiple owners
are rejected rather than partially committed or coordinated with two-phase
commit.

## Consequences

- RedDB can express the Shopify-like reservation shape without requiring Redis
  as a coordination sidecar.
- The primitive depends on record-level claim-locking beneath the current
  collection-level runtime lock adapter.
- Query planning must distinguish deterministic index-backed candidate ordering
  from incidental scan order or unbounded sort.
- `Exact claim` is all-or-nothing over immediately claimable records, not over
  possible future records or records currently locked by other transactions.
- Claim planning must apply read visibility before acquiring candidates and
  update authorization before publishing the state transition.
- Range claims remain snapshot-isolation operations. Users needing full
  serializable predicate protection need a future SSI/predicate-lock design, not
  hidden behavior in claim.
- Claim contention, skipped candidates, and claim misses are normal hot-path
  signals. They belong in metrics/tracing, not per-attempt audit logs.
- Queue claims must route through `QueueLifecycle`; duplicating queue delivery
  decisions would violate ADR 0020.
- Cluster claims stay owner-local until RedDB has a separate distributed write
  transaction decision.

## Alternatives considered

- **Lock-only `SELECT ... FOR UPDATE SKIP LOCKED`.** Rejected for the first
  product shape because it exposes a low-level lock act while the business need
  is a state transition.
- **Allow non-indexed `ORDER BY`.** Rejected for the first contract because a
  claim is a concurrent write path; broad scans and sorts are likely to create
  the bottleneck this primitive exists to avoid.
- **Persistent lease fields in user data.** Rejected as the primitive contract
  because it pollutes every model with application-visible lock metadata and
  makes crash/expiry semantics part of user schema.
- **Optimistic conflict-only claim.** Rejected because it does not provide true
  skip-locked progress; many transactions can choose the same candidate and only
  discover the race at commit.
- **Predicate/gap locking in the first cut.** Rejected because it would smuggle
  serializable-style behavior into a snapshot-isolation engine.
- **Raw claim over queue internals.** Rejected because `QueueLifecycle` is the
  queue state-machine authority.
- **Cross-owner claim in v1.** Rejected because exact cross-owner reservation is
  a distributed transaction problem.
- **Built-in idempotency clause.** Rejected for the primitive; idempotency is
  required by the canonical recipe but stays modeled as ordinary transactional
  data.

## References

- ADR 0014: mvcc-history-store-and-transaction-recovery
- ADR 0015: events-dual-write-window
- ADR 0020: queue-lifecycle-module-contract
- ADR 0037: shard-range-ownership-catalog
- ADR 0055: cluster-slot-map-and-cross-shard-operations
- `.red/context/data-model.md`: Resource reservation, Concurrent claim, Claim
  lock, Claim identity, Exact claim, Claim miss, Claim authority, Owner-local
  claim, Claim authorization, Claim order, Reservation idempotency key
