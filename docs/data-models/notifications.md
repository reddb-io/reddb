# Ephemeral Notifications

RedDB ships three distinct event-flow primitives. The boundary
between them is intentional — collapsing them into one overloaded
abstraction would force users to opt out of behavior they didn't
ask for. See [ADR 0028](../../.red/adr/0028-live-queue-notification-stream-boundaries.md)
for the architectural rationale.

| Primitive                  | Replay | ACK / DLQ | Consumer offsets | Per-message state              | Typical use                                  |
| -------------------------- | :----: | :-------: | :--------------: | ------------------------------ | -------------------------------------------- |
| **Queue**                  |   N    |     Y     |        N         | Pending delivery, ACK, retries | Work distribution, durable jobs              |
| **Durable stream**         |   Y    |     N     |        Y         | Append-only log + offsets      | Event sourcing, audit logs, future CDC       |
| **Ephemeral notification** |   N    |     N     |        N         | None — fire and forget         | Live UI hints, "deployed", "config reloaded" |

This page documents the **ephemeral notification** primitive
landed by issue #720 against PRD #718.

## Contract

An ephemeral notification is a fire-and-forget signal on a
tenant-scoped channel. The engine never persists, replays, ACKs,
or buffers notifications for offline listeners. A notification
that arrives while a listener is disconnected is gone — by design.
If your use case needs durability, use a queue or a durable stream.

### Scope

Notification channels live inside a tenant by default.
`tenant=acme, channel=deploys` and `tenant=globex, channel=deploys`
are completely independent: a publish to one is invisible to
listeners on the other. The platform tenant (`tenant=None` in the
auth model) has its own global namespace.

Crossing the tenant boundary — either publishing to another
tenant's channel or subscribing to one — requires the
`notify:cross-tenant` capability. Same-tenant operations require
only `notify` (or a broader `*` policy that covers it).

### No replay

The registry is built on per-channel broadcast channels. A
subscriber's cursor starts at the channel's *current tail*, so:

- Messages published before a subscriber connects are not
  delivered.
- A subscriber that disconnects and reconnects re-enters at the
  current tail and observes only future messages.
- Channels with no active listeners drop messages silently and
  reap their underlying capacity.

This is the deliberate trade-off: ephemeral signals do not
accumulate in memory for absent consumers. Durable behavior
belongs to queues and streams.

## Authorization model

The notification registry does not consult policies directly —
it asks the calling transport "does this principal hold the
cross-tenant capability?" and trusts the answer. Transports
evaluate the policy gate before calling
`NotificationRegistry::publish_authorized` or
`subscribe_authorized`. This mirrors the AiProviderGate pattern
from PRD #711.

Two actions are added to the action catalog:

- `notify` — publish to / subscribe to channels in the
  principal's own tenant. Required for every notification
  operation.
- `notify:cross-tenant` — additionally required when the
  target scope differs from the principal's tenant (including
  the platform-global namespace).

Operators who want to forbid cross-tenant notifications can rely
on the default: without an explicit allow, the registry returns
`NotificationError::CrossTenantDenied`.

## PG-like LISTEN / NOTIFY compatibility

PG-wire `LISTEN <channel>` and `NOTIFY <channel> [, '<payload>']`
translate onto the same registry: the PG-wire handler resolves
the session's tenant binding, derives a `NotificationScope`, and
calls `subscribe_authorized` / `publish_authorized`. The
translation never touches queue wait semantics — queues remain
a separate primitive with their own delivery state machine and
their own SQL surface.

The canonical RedDB-native contract is the runtime API in
`reddb_server::notifications`. PG-wire forms are an
interoperability layer for existing client libraries, not the
source of truth.

## Transport availability

The RedDB-native notification API (`subscribe_authorized` /
`publish_authorized`) is reachable from HTTP, RedWire, gRPC, and Postgres-wire
within a single process. PG-wire additionally translates `LISTEN <channel>`
and `NOTIFY <channel> [, '<payload>']` onto the same registry — that
compatibility surface is PG-wire-only by design. See
[Event Workflow](event-workflow.md#transport-availability) for the matrix and
[Postgres-wire reference](../api/postgres-wire.md#event-workflow-primitives)
for the `LISTEN` / `NOTIFY` mapping.

## What this primitive is NOT

- **Not a replacement for `QUEUE READ … WAIT`** — that primitive
  preserves ACK/NACK/DLQ and per-message delivery state.
- **Not a durable stream** — durable streams keep per-consumer
  offsets and can replay history. See the dedicated
  [durable stream primitive](streams.md).
- **Not a cross-process pub/sub bus** — the registry is
  in-memory and process-local. Multi-node fan-out is out of
  scope for the first slice (mirrors the ADR's decision to defer
  cross-node queue wake).
