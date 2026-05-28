# Event Workflow Primitives

RedDB ships three distinct event-flow primitives — **queues**, **durable
streams**, and **ephemeral notifications** — instead of one overloaded
abstraction. The split is architectural, not cosmetic: each primitive has its
own delivery state machine, and forcing them into one shape would force users
to opt out of behavior they didn't ask for. See
[ADR 0028](../../.red/adr/0028-live-queue-notification-stream-boundaries.md)
for the rationale.

This page is the integration map across the three primitives, the transports
that surface them, and the Honker-inspired roadmap that scoped them in
[PRD #718](https://github.com/reddb-io/reddb/issues/718).

## Choosing a primitive

| Need                                                       | Primitive                              | Page                              |
| ---------------------------------------------------------- | -------------------------------------- | --------------------------------- |
| Work distribution with ACK / NACK, retries, DLQ            | **Queue** (WORK mode)                  | [Queues](queues.md)               |
| Broadcast to every consumer with ACK / DLQ per consumer    | **Queue** (FANOUT mode)                | [Queues](queues.md)               |
| Block a worker until a message arrives (no busy poll)      | **Queue** + `QUEUE READ ... WAIT`      | [Queues](queues.md#configuration) |
| Schedule a job for future delivery                         | **Queue** + `DELAY` / `AVAILABLE AT`   | [Queues](queues.md#push-pop)      |
| Retry policy with per-failure delay override               | **Queue** + `RETRY_DELAY` / `WITH DLQ` | [Queues](queues.md#retry-policy)  |
| Append-only event log with replay and per-consumer offsets | **Durable stream**                     | [Streams](streams.md)             |
| Fire-and-forget signal — live UI hints, "config reloaded"  | **Ephemeral notification**             | [Notifications](notifications.md) |
| Mirror collection mutations into a queue                   | **Events** + queue target              | [Events](events.md)               |

The boundary table on each page is the same — read it once and it pins the
decision:

| Primitive                  | Replay | ACK / DLQ | Consumer offsets | Per-message state              | Typical use                                  |
| -------------------------- | :----: | :-------: | :--------------: | ------------------------------ | -------------------------------------------- |
| **Queue**                  |   N    |     Y     |        N         | Pending delivery, ACK, retries | Work distribution, durable jobs              |
| **Durable stream**         |   Y    |     N     |        Y         | Append-only log + offsets      | Event sourcing, audit logs, future CDC       |
| **Ephemeral notification** |   N    |     N     |        N         | None — fire and forget         | Live UI hints, "deployed", "config reloaded" |

A queue read creates pending delivery state. A stream read advances or records
an offset. A notification has no replay state at all. These state machines do
not collapse cleanly — that is the whole point.

## Transport availability

Every public primitive ships over the four supported transports unless noted.
The semantic result shape (messages on success, zero records on timeout,
explicit error on cancellation, explicit error above the wait cap) is the same
across transports — switching wires does not change worker behavior.

| Capability                                          | HTTP | RedWire | gRPC | Postgres-wire |
| --------------------------------------------------- | :--: | :-----: | :--: | :-----------: |
| `QUEUE PUSH` / `QUEUE READ` / `QUEUE ACK` / `NACK`  |  ✅  |   ✅    |  ✅  |      ✅       |
| `QUEUE READ ... WAIT <duration>` (live queue wait)  |  ✅  |   ✅    |  ✅  |      ✅       |
| `QUEUE PUSH ... DELAY` / `AVAILABLE AT`             |  ✅  |   ✅    |  ✅  |      ✅       |
| `QUEUE NACK ... WITH DELAY` (per-failure override)  |  ✅  |   ✅    |  ✅  |      ✅       |
| `QUEUE MOVE` / `SELECT ... FROM QUEUE` (inspection) |  ✅  |   ✅    |  ✅  |      ✅       |
| Durable stream primitive (registry API)             |  —   |    —    |  —   |       —       |
| Durable stream DDL / wire surface                   |  🟡  |   🟡    |  🟡  |      🟡       |
| Ephemeral notification (RedDB-native API)           |  ✅  |   ✅    |  ✅  |      ✅       |
| Ephemeral notification via `LISTEN` / `NOTIFY`      |  —   |    —    |  —   |      ✅       |

Legend: ✅ shipped · 🟡 deferred behind a later slice of PRD #718 · — not
applicable on that transport.

The durable stream primitive landed as an in-process registry under issue
#721; the public DDL surface (`CREATE STREAM`, `STREAM APPEND`,
`STREAM READ FROM`) and the four-transport wire bindings are deliberately
deferred to a later slice. The registry contract is stable so transports
can wire on top without further changes to the public type.

PG-wire `LISTEN` / `NOTIFY` translates onto the ephemeral notification
primitive — see [PG-like LISTEN / NOTIFY compatibility](notifications.md#pg-like-listen--notify-compatibility)
and the [Postgres-wire reference](../api/postgres-wire.md#event-workflow-primitives).
It does **not** map to `QUEUE READ ... WAIT`: queue wait preserves
ACK/NACK/DLQ and per-message delivery state, which notifications do not have.

## Honker migration — what RedDB took, what it left

PRD #718 named Honker as the comparison point and chose primitives by fit, not
by surface-area parity. The split that landed:

| Honker concept                              | RedDB outcome                                                                                   |
| ------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Live worker waiting                         | **Adopted** as `QUEUE READ ... WAIT <duration>` on the existing queue lifecycle.                |
| Delayed job availability                    | **Adopted** as `QUEUE PUSH ... DELAY` / `AVAILABLE AT` per message.                             |
| Declarative retry / backoff                 | **Adopted** as queue-level `RETRY_DELAY` + `MAX_ATTEMPTS` + `WITH DLQ`, with `NACK ... WITH DELAY` override. |
| Ephemeral pub / sub signals                 | **Adopted** as a separate ephemeral notification primitive — never as a queue mode.             |
| Durable streams with per-consumer offsets   | **Adopted** as a separate `Durable stream` collection model with monotonic offsets.             |
| Named locks                                 | **Out of scope.** Not a queue/stream/notification semantic.                                     |
| Public rate-limiting primitive              | **Out of scope.** Separate coordination/infra concern, not in PRD #718.                         |
| Scheduler / cron                            | **Out of scope.** No PRD has been written for this yet.                                         |
| Task result storage / `WAIT RESULT`         | **Out of scope.** Not in PRD #718; no later PRD covers it.                                      |
| Job get-by-id / cancel API                  | **Out of scope.** Workers can use queue inspection (`SELECT ... FROM QUEUE`) instead.           |

The architectural reason these were left out is the same in every case:
queue, stream, and notification state machines should not absorb unrelated
coordination primitives. A named lock is not a queue read; a rate limiter is
not a stream offset; a cron tick is not an ephemeral notification. Picking
those up later requires its own PRD — naming the primitive, scoping the
state, and recording the boundary in an ADR — not a queue extension.

## Out of scope (current PRD)

These items are explicitly **not** delivered by PRD #718 and have no later
PRD that picks them up today. Treat them as missing primitives, not as
features hidden behind a flag:

- **Named locks** — no `LOCK` / `UNLOCK` primitive on any transport.
- **Public rate limiting** — no token-bucket / leaky-bucket primitive
  exposed to clients. Internal admission control is unrelated.
- **Scheduler / cron** — RedDB does not expose recurring schedules. Use
  `QUEUE PUSH ... AVAILABLE AT <instant>` from an external scheduler if you
  need one-shot timing.
- **Task result storage / `WAIT RESULT`** — `QUEUE READ ... WAIT` returns the
  message envelope, not a worker-produced result. Result handoff is the
  application's responsibility.
- **Job get-by-id / cancel** — no `QUEUE CANCEL <id>` or `QUEUE GET <id>`
  command. `SELECT ... FROM QUEUE` is read-only inspection; cancellation is
  done by ACKing or NACKing through the normal consumer-group path.
- **Cross-node queue wake** — the first wait slice wakes local waiters only.
  Cluster-aware wake is a deferred performance slice.

If any of these gaps become a real product requirement they will get their
own PRD, glossary entry, and ADR before code lands — not a quiet extension
of an existing primitive.

## See also

- [Queues](queues.md) — live queue wait, delayed messages, retry policy, DLQ.
- [Notifications](notifications.md) — ephemeral, tenant-scoped pub / sub.
- [Streams](streams.md) — durable append-only logs with per-consumer offsets.
- [Events](events.md) — collection mutations that target queues.
- [Postgres-wire](../api/postgres-wire.md#event-workflow-primitives) — queue
  wait and `LISTEN` / `NOTIFY` mapping.
- [ADR 0028](../../.red/adr/0028-live-queue-notification-stream-boundaries.md) —
  the architectural boundary.
- [PRD #718](https://github.com/reddb-io/reddb/issues/718) — the roadmap
  these slices implement.
