# ADR 0072: Queue Ordering Keys And Push Idempotency Keys

Status: accepted
Date: 2026-07-05

## Context

ADR 0020 pinned queue delivery to "at-least-once with no order; users wanting
FIFO use a single consumer." That is all-or-nothing: ordering one entity
(deposit-before-withdraw on `account_123`) requires serializing the entire
queue. Separately, at-least-once pushes the whole idempotency burden onto
consumers; producer retries after network timeouts have no safe primitive.

This ADR adds two per-message identities to `QUEUE PUSH`. They are designed
together — the decision to keep them separate identities (rather than one
overloaded key, as some systems do) is itself one of the decisions — but they
are independently usable and independently implementable.

## Decision

### Ordering key

- `QUEUE PUSH ... KEY 'x'` serializes delivery among messages sharing the
  key: at most one pending delivery per `(consumer group, ordering key)`, in
  both `WORK` and `FANOUT` modes. Blocking state is indexed per group — a
  slow or failing group never blocks another group's progress on the same key.
- **Per-message opt-in, no DDL.** Keyless messages on the same queue behave
  exactly as today (no order, no blocking). Mode stays data, not a type
  parameter (ADR 0020); there is no `KEYED` queue flavor.
- **Strict under failure.** A NACKed/retrying message blocks its key for that
  group until it is ACKed or retired to the DLQ. DLQ promotion unblocks the
  key. Worst-case block is bounded: `max_attempts × retry_delay`. The
  motivating scenario needs the guarantee precisely when deliveries fail;
  best-effort-under-failure is not ordering.
- **Mutual exclusions at push time.** `KEY` is rejected in combination with
  `DELAY`/`AT` (a delayed message would either freeze its key or dilute the
  guarantee to "ordered except when not") and on `PRIORITY` queues (priority
  reorders by definition). Internal retry delay is not user delay — it blocks
  the key, which is the strictness contract, not a contradiction of it.
- **The key travels.** DLQ promotion and `QUEUE MOVE` preserve the key as
  message metadata (the DLQ is a regular queue; enqueue recurses — ADR 0012 /
  0020). A replayed message joins the tail of its key at the destination; the
  original position is gone by definition, and replay asserts "process this
  again, now."
- **Grouped delivery only.** Raw deque primitives (`QUEUE POP`, either side)
  sit outside the guarantee — `RPOP` is structurally order-hostile, and
  `pop_available` already cannot steal in-flight messages. Mixing POP with
  ordered consumption on one queue is a documented user error, not policed.
- Canonical term: **Ordering key** (Pub/Sub vocabulary). Not "message group"
  (collides with consumer group) and not "partition key" (implies sharding we
  do not have; the name stays free for a future hash-by-key slot-map
  follow-up to ADR 0055).

### Push idempotency key

- `QUEUE PUSH ... DEDUP 'id'` makes producer retries idempotent within a
  queue-scoped dedup window: a duplicate push inside the window is a no-op
  returning the original message id — success, identical to the first push,
  whether or not the original was already consumed.
- **Window is a queue property.** Engine default (SQS-style, minutes-order),
  adjustable via `DEDUP_WINDOW` on `CREATE`/`ALTER QUEUE`. Pushes never carry
  window durations — producers choosing divergent windows on one queue would
  make "is this deduped?" incoherent.
- **First-committer-wins participant.** The dedup key is a write subject to
  conflict under SI+FCW: of two concurrent transactions pushing the same key,
  the second aborts at commit; its retry observes the committed outcome and
  receives the idempotent no-op. Dedup therefore composes with transactional
  enqueue (the outbox pattern) instead of being autocommit-only.
- **Durable, lazily evicted.** The index is WAL-durable — crash plus producer
  retry must not reopen the duplicate window. Window expiry is reclaimed
  lazily on push; no background sweeper (ADR 0020 house rule).
- **Producer-PUSH boundary only.** The index is written and consulted
  exclusively at `QUEUE PUSH ... DEDUP`. DLQ promotion, retry re-timing, and
  operator `QUEUE MOVE` bypass it — an operator replay silently no-oping as a
  duplicate of its own original push would be the worst possible failure mode
  for a triage tool. The feature targets producer retries, not global message
  uniqueness.
- Canonical term: **Push idempotency key**, sibling of the existing
  **Reservation idempotency key** (ADR 0063) — same retried-request-observes-
  existing-outcome semantics, one vocabulary family.

### Shared surface

The two identities are orthogonal and freely combinable on one push. RQL
keeps distinct verbs (`KEY` / `DEDUP`) so neither concept borrows the other's
name — SQS's `MessageGroupId` / `MessageDeduplicationId` split, with shorter
spelling.

## Consequences

- The ADR 0020 clause "at-least-once with no order; FIFO users run a single
  consumer" is superseded for keyed messages; unkeyed traffic keeps the old
  contract verbatim.
- Delivery gains a skip-scan: a message is undeliverable while its
  `(group, key)` has an in-flight pending delivery. Pending state grows a
  per-(group, key) in-flight index; replicas replay outcomes as before —
  primary decides, nothing new crosses the replication contract (ADR 0020).
- A poisoned keyed message throttles its key (bounded by retry policy) until
  DLQ retirement — that is the contract working, and the DLQ + `QUEUE MOVE`
  loop is the recovery path.
- Wire drivers gain two optional push parameters; introspection surfaces
  (`red.queue_pending`, `SELECT ... FROM QUEUE` projection) grow a `key`
  column.
- Dedup adds a WAL-durable index whose size is bounded by push rate × window.

## Alternatives considered

- **Overtake-on-retry ordering** (retry to tail, key continues). Rejected:
  destroys the guarantee exactly in the failure scenario that motivates it.
- **Queue-level `KEYED` mode** (SQS FIFO shape). Rejected: forces migration
  and forbids the motivating mixed-traffic case; per-message opt-in follows
  ADR 0020's mode-is-data principle.
- **`DELAY` within a key** (block the key, or order-by-availability).
  Rejected both ways: silent multi-hour key freezes vs. a diluted contract.
- **`WORK`-only v1.** Rejected: in-flight state is keyed by `(group, key)`
  anyway, so `FANOUT` generalizes for free, and ordered fan-out (every
  subscriber group sees an entity's events in order) is the strongest case.
- **Strip the key at DLQ promotion.** Rejected: the key is forensic triage
  data and preservation is the zero-code default.
- **Error on duplicate push.** Rejected: turns every legitimate retry into a
  producer-side try/catch; contradicts the Reservation-idempotency precedent.
- **Per-push dedup windows.** Rejected: incoherent per-queue semantics.
- **Autocommit-only `DEDUP`.** Rejected: amputates dedup from transactional
  enqueue, weakening both features.
- **Dedup on every enqueue path.** Rejected: silent no-op replays.
- **POP inside the ordering contract** (implicit group, or reject keyed
  pops). Rejected: `RPOP` breaks the model structurally; data-dependent POP
  behavior surprises operators.

## References

- ADR 0012: queue DLQ replay and read-only queue projection
- ADR 0020: QueueLifecycle module contract (superseded in part — the
  no-order clause — for keyed messages)
- ADR 0026: delivery-id wire shape
- ADR 0028: live queue, notification, and stream boundaries
- ADR 0055: cluster slot map (queues slot by name; hash-by-key sharding is a
  potential follow-up enabled by this vocabulary)
- ADR 0063: concurrent claim / resource reservation (idempotency-key family)
- `.red/context/data-model.md` → **Ordering key**, **Push idempotency key**
