# Durable Streams

RedDB ships three distinct event-flow primitives. The boundary
between them is intentional — collapsing them into one overloaded
abstraction would force users to opt out of behavior they didn't
ask for. See
[ADR 0028](../../.red/adr/0028-live-queue-notification-stream-boundaries.md)
for the architectural rationale.

| Primitive                  | Replay | ACK / DLQ | Consumer offsets | Per-message state              | Typical use                                  |
| -------------------------- | :----: | :-------: | :--------------: | ------------------------------ | -------------------------------------------- |
| **Queue**                  |   N    |     Y     |        N         | Pending delivery, ACK, retries | Work distribution, durable jobs              |
| **Durable stream**         |   Y    |     N     |        Y         | Append-only log + offsets      | Event sourcing, audit logs, future CDC       |
| **Ephemeral notification** |   N    |     N     |        N         | None — fire and forget         | Live UI hints, "deployed", "config reloaded" |

This page documents the **durable stream** primitive landed by
issue #721 against PRD #718.

## Contract

A durable stream is an append-only event log. The engine assigns a
monotonic `offset: u64` to each appended event and never reuses or
rewrites those offsets — even retention pruning only advances the
head, leaving surviving events at their original positions.
Consumers read by supplying an offset window and save their own
progress separately. The engine never tracks per-message delivery
state, never times out a delivery, never requires ACK or NACK, and
never moves a message to a dead-letter queue. If your use case
needs any of that, you want a queue, not a stream.

### Event shape

```rust
pub struct StreamEvent {
    pub scope: StreamScope,
    pub stream: String,
    pub key: Option<String>,   // optional stream-identity / row-key
    pub payload: String,       // opaque UTF-8 (typically JSON)
    pub offset: u64,           // engine-assigned, monotonic
    pub appended_at_ms: u128,
}
```

`key` is opaque to the engine in this slice. It exists so that a
future materialized-CDC slice can populate streams from a table's
mutation tail with `key = primary key` and `payload = row JSON`
without changing the public event type.

### Append

```rust
let offset = registry.append(
    StreamScope::Tenant("acme".into()),
    "orders",
    Some("order:42".into()),
    r#"{"status":"placed"}"#,
    now_ms,
)?;
// offset == 1 for the first append, 2 for the next, ...
```

### Read

`read_since(scope, name, from, limit)` returns up to `limit` events
with `offset >= from`. Pure read — does not consume, lease, advance
the consumer's saved offset, or leave pending delivery state
behind. Re-reading the same window returns the same events.

```rust
let events = registry.read_since(&scope, "orders", saved + 1, 100)?;
```

If `from` is below the current head (because retention has pruned
older events), the returned slice simply starts at the head with
no error. Operators detect lag by comparing `get_offset(consumer)`
against the descriptor's `head_offset`.

### Per-consumer offsets

```rust
registry.save_offset(&scope, "orders", "billing-svc", last_processed)?;
let resume_from = registry.get_offset(&scope, "orders", "billing-svc")? + 1;
```

`save_offset` is **monotonic**: a smaller or equal offset is
dropped silently and the previously-saved value is returned. This
makes the operation safe to retry on duplicate or stale "I'm done
with offset N" notifications — a consumer can never rewind past
events it already finished. `get_offset` returns `0` for consumers
that have never saved (offset `0` is the reserved "no progress
yet" sentinel since the first real event is at offset `1`).

### Discovery

```rust
registry.create_stream(scope, "orders", StreamRetention::default())?;
let listed: Vec<StreamDescriptor> = registry.list_streams(&scope);
let one: Option<StreamDescriptor> = registry.describe(&scope, "orders");
```

`StreamDescriptor` carries the stream's name, retention contract,
`head_offset`, `tail_offset`, and `event_count`. A future
`red.streams` virtual table will project the same fields; the SQL
DDL (`CREATE STREAM`, `STREAM APPEND`, `STREAM READ FROM`) and
HTTP/PG-wire transport bindings are deliberately deferred to a
later slice — this slice ships the primitive and its authorization
model so transports can wire on top without further changes to the
registry contract.

## Retention contract (first cut)

Each stream carries a `StreamRetention { max_events, max_age_ms }`
declaration. Both fields are independently optional; the default
is an unbounded log.

- `max_events: Option<usize>` — drop the oldest events so the log
  never exceeds N entries.
- `max_age_ms: Option<u64>` — drop events older than
  `now - max_age_ms`.

Caps compose by AND (the stricter one wins). Retention runs at
append time, after the new event is inserted, so the just-appended
event is always retained even when it pushes the head past the cap.
The pass never rewrites the offset of surviving events — offsets
remain sparse once the head moves forward.

Consumer offsets are **not** reset by retention. A consumer whose
saved offset has fallen below the current head will simply skip
the truncated prefix on the next `read_since` call; the engine
does not raise an error for "consumer lagged past retention".
Operators who care about that condition should monitor
`descriptor.head_offset - get_offset(consumer)` themselves.

## Authorization model

Mirrors the [notification primitive](notifications.md): the
registry does not consult policies directly — it asks the calling
transport "does this principal hold the cross-tenant capability?"
and trusts the answer. Transports evaluate the policy gate before
calling `*_authorized` entry points.

Two actions are added to the action catalog:

- `stream` — append, read, and offset-save on streams in the
  principal's own tenant. Required for every stream operation.
- `stream:cross-tenant` — additionally required when the target
  scope differs from the principal's tenant (including the
  platform-global namespace).

Operators who want to forbid cross-tenant stream access can rely
on the default: without an explicit allow, the registry returns
`StreamError::CrossTenantDenied`.

## Future materialized CDC

Materialized change-data-capture — populating a stream from a
table's mutation tail with one event per row change — is
intentionally future work. The current event shape (`scope`,
`stream`, `key`, `payload`, `offset`, `appended_at_ms`) is the
standard CDC log shape, so a later slice can light it up without
changing the public type. The `WITH EVENTS … TARGET STREAM <name>`
DDL form, the on-startup catch-up scan, and the
exactly-once-per-table-mutation guarantee are all deferred.

## Transport availability

The durable stream primitive landed as an in-process registry. The public
SQL DDL (`CREATE STREAM`, `STREAM APPEND`, `STREAM READ FROM`) and the
HTTP / RedWire / gRPC / Postgres-wire bindings are deliberately deferred to a
later slice of PRD #718 — the registry contract is stable so transports can
wire on top without further changes to the public event type. See
[Event Workflow](event-workflow.md#transport-availability) for the cross-
primitive matrix.

Streams remain distinct from queues on the wire as well as in semantics: a
queue read produces pending delivery state, while a stream read advances or
records an offset. A future transport binding will preserve that boundary
rather than overload queue commands.

## What this primitive is NOT

- **Not a queue** — no per-message delivery state, no ACK/NACK,
  no DLQ, no timeout-driven redelivery. Use a queue if any of
  that matters.
- **Not an ephemeral notification** — streams are durable and
  replayable; the consumer's offset is the contract for "where I
  am". Offline-replay use cases that *don't* need durability
  should use [ephemeral notifications](notifications.md) instead.
- **Not a cross-process bus** — this slice is in-process. A
  follow-up slice under the same PRD wires the log to disk-backed
  storage; the public contract above does not change.
