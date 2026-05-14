# Events

Events connect ordinary collections to RedDB queues. An event-enabled collection
emits a JSON change envelope for each matching mutation, and subscribers consume
those envelopes with the normal queue commands.

Use events when a downstream system needs change data without application
dual-writes: audit logs, search indexing, cache invalidation, analytics sync,
webhook fanout, and tenant-scoped operational monitoring.

## Current Status

The implemented surface covers `WITH EVENTS`, auto-created queues, explicit
target queues, operation filters, `WHERE` filters, `REDACT`, named
subscriptions, tenant isolation, cycle prevention, mutation events, truncate and
drop events, outbox DLQ routing, and schema-evolution operator alerts.

Two related slices are still open:

- `EVENTS BACKFILL ...` and `synthetic: true` events are blocked by issue #300.
- `red.subscriptions` and `EVENTS STATUS` are blocked by issue #303.

This page documents the intended contract for both features, but the runnable
quickstart and conformance corpus avoid their syntax until those slices land.

## Quick Start

Create a table that emits to the default queue:

```sql
CREATE TABLE users (id INT, email TEXT, name TEXT) WITH EVENTS;
INSERT INTO users (id, email, name) VALUES (1, 'ada@example.com', 'Ada');
QUEUE GROUP CREATE users_events sync_workers;
QUEUE READ users_events GROUP sync_workers CONSUMER search_indexer COUNT 10;
QUEUE ACK users_events GROUP sync_workers 'msg-id-123';
```

When `TO` is omitted, RedDB creates `<collection>_events` as a `FANOUT` queue.
That queue is visible in `SHOW COLLECTIONS` and `SHOW QUEUES`.

Use a custom queue when several collections should feed one downstream pipe:

```sql
CREATE QUEUE audit_log FANOUT;
CREATE TABLE accounts (id INT, email TEXT, phone TEXT, status TEXT)
  WITH EVENTS (INSERT, UPDATE, DELETE)
  TO audit_log
  REDACT (email, phone)
  WHERE status = 'active';
QUEUE GROUP CREATE audit_log auditors;
QUEUE READ audit_log GROUP auditors CONSUMER compliance COUNT 25;
```

The source collection and target queue remain separate authorization resources:
creating the subscription requires read permission on the source and write
permission on the target queue, while consumers still need queue read access.

## Declaration Syntax

```sql
CREATE TABLE users (id INT, email TEXT) WITH EVENTS;
CREATE TABLE users (id INT, email TEXT) WITH EVENTS TO audit_log;
CREATE TABLE users (id INT, email TEXT) WITH EVENTS (INSERT, UPDATE);
CREATE TABLE users (id INT, email TEXT) WITH EVENTS REDACT (email);
CREATE TABLE users (id INT, email TEXT, status TEXT) WITH EVENTS WHERE status = 'active';
```

`ALTER TABLE` can add, re-enable, disable, or remove subscriptions:

```sql
ALTER TABLE users ENABLE EVENTS TO audit_log;
ALTER TABLE users ENABLE EVENTS (DELETE) TO audit_log;
ALTER TABLE users DISABLE EVENTS;
ALTER TABLE users ADD SUBSCRIPTION analytics TO warehouse_events WHERE status = 'active';
ALTER TABLE users ADD SUBSCRIPTION masked TO pii_events REDACT (email, phone);
ALTER TABLE users DROP SUBSCRIPTION analytics;
```

Queues never emit events. RedDB rejects this to prevent loops:

```sql
CREATE QUEUE audit_log WITH EVENTS;
```

The engine also rejects subscription graphs that would create a cycle.

## Payload Spec

Each queue message stores the event payload as JSON:

```json
{
  "event_id": "opaque-deterministic-id",
  "op": "update",
  "collection": "users",
  "id": 42,
  "ts": 1715200000000,
  "lsn": 98234,
  "tenant": "acme",
  "synthetic": false,
  "before": {"name": "Ada"},
  "after": {"name": "Ada Lovelace"}
}
```

Fields:

| Field | Meaning |
|---|---|
| `event_id` | Deterministic id for consumer deduplication. It is derived from `collection`, `id`, `lsn`, and `op`; treat it as opaque. |
| `op` | `insert`, `update`, `delete`, `truncate`, or `collection_dropped`. |
| `collection` | Source collection name. |
| `id` | User primary key when available, otherwise RedDB's synthetic entity id. |
| `ts` | Server timestamp in Unix milliseconds. |
| `lsn` | Per-collection ordering key from the mutation log. |
| `tenant` | Active tenant id, or `null` for unscoped/admin execution. |
| `synthetic` | `true` for BACKFILL events once issue #300 lands; ordinary mutation events are real-time. |
| `before` | Previous row/document state for update/delete, otherwise `null` or omitted for collection-level ops. |
| `after` | New row/document state for insert/update, otherwise `null` or omitted for collection-level ops. |

Consumers should order by `lsn` within a collection and deduplicate by
`event_id`.

## Operations

| Operation | Event behavior |
|---|---|
| `INSERT` | One `insert` event per inserted entity. |
| `UPDATE` | One `update` event per changed entity, with `before` and `after`. |
| `DELETE` | One `delete` event per deleted entity, with `before`. |
| `TRUNCATE` | One `truncate` event for the collection, not one event per removed row. |
| `DROP` | One `collection_dropped` event; the queue remains available for draining. |
| Replication apply | Does not emit events on replicas, preventing primary/replica duplicates. |
| Queue mutation | Queues receive events but do not emit their own events. |

## REDACT

`REDACT` removes fields at producer time before the payload reaches the queue:

```sql
CREATE TABLE users (id INT, email TEXT, phone TEXT, name TEXT)
  WITH EVENTS TO audit_log REDACT (email, phone);
```

Redaction applies independently per subscription and to both `before` and
`after`. Dotted JSON paths and wildcard path segments are supported:

```sql
CREATE TABLE docs (id INT, body JSON)
  WITH EVENTS TO audit_log REDACT (body.user.email);
CREATE TABLE messages (id INT, body JSON)
  WITH EVENTS TO audit_log REDACT (body.*.email);
```

If policies deny selecting a sensitive column but the subscription does not
redact that column, RedDB emits a DDL warning so operators can fix the
subscription before data leaves the source collection.

## Filters And Multiple Subscriptions

Operation filters reduce event volume:

```sql
CREATE TABLE orders (id INT, status TEXT, amount INT)
  WITH EVENTS (INSERT, UPDATE) TO order_changes;
```

`WHERE` filters evaluate against the row state relevant to the operation:

```sql
CREATE TABLE orders (id INT, status TEXT, amount INT)
  WITH EVENTS TO paid_orders WHERE status = 'paid';
```

A collection can have multiple named subscriptions:

```sql
ALTER TABLE orders ADD SUBSCRIPTION audit TO audit_log;
ALTER TABLE orders ADD SUBSCRIPTION search TO search_sync WHERE status = 'paid';
ALTER TABLE orders ADD SUBSCRIPTION masked TO pii_events REDACT (customer_email);
ALTER TABLE orders DROP SUBSCRIPTION search;
```

Each subscription has its own target queue, filters, and redaction list.

## Tenant Isolation

Events follow the active tenant scope. For tenant-scoped subscriptions, a
mutation in tenant `acme` routes to the tenant-scoped target queue and does not
appear in another tenant's queue. The payload also carries `"tenant": "acme"`.

Cluster-wide subscriptions require privileged authoring:

```sql
ALTER TABLE users ENABLE EVENTS TO global_audit ON ALL TENANTS;
```

Tenant-scoped users cannot create `ON ALL TENANTS` subscriptions unless they
hold the cluster event-subscribe capability.

## Backpressure And DLQ

Current delivery writes the source mutation first and then enqueues the event
payload to the target queue. These writes are separate store WAL batches in
autocommit mode, so a crash between them can leave a durable row without its
event. See [ADR 0015](../adr/0015-events-dual-write-window.md). The intended
direction is a true internal outbox or same-batch queue write.

If the target queue is full or the outbox exceeds configured pressure limits,
RedDB routes the payload to `<queue>_outbox_dlq`, emits an operator event, and
updates Prometheus counters:

```text
reddb_events_enqueued_total
reddb_events_drain_retries_total{reason="queue_full"}
reddb_events_dlq_total
```

Operators can inspect the DLQ with normal queue reads and decide whether to
replay, repair, or discard each payload.

## Schema Evolution

DDL does not emit ordinary collection events. When an event-enabled collection
changes shape, RedDB emits an operator-grade schema-evolution alert so downstream
owners can update rigid schemas before consuming the next payload shape.

Payload consumers should treat unknown fields as additive and should not assume
that every `after` object has the same set of keys forever.

## Backfill Pattern

Backfill is the bootstrap/replay path for existing rows:

```sql
EVENTS BACKFILL users WHERE created_at >= '2026-01-01' TO audit_log LIMIT 10000;
```

Backfill events carry `synthetic: true`, use deterministic `event_id` values so
reruns are idempotent, honor tenant scope, and apply the target subscription's
redaction rules.

`EVENTS BACKFILL STATUS <collection>` is reserved for a later progress-tracking
slice; the current runtime returns an explicit not-implemented error for that
status command.

## Consumer Pattern

1. Read from the event queue with a durable group.
2. Validate the payload schema version your service understands.
3. Deduplicate by `event_id`.
4. Apply changes in `lsn` order for each source collection.
5. Acknowledge only after the downstream write commits.
6. On transient errors, leave the message pending or `NACK` it.
7. On poison payloads, move the item to an application DLQ or alert an operator.

## Comparison

| Capability | RedDB Events | Postgres logical replication | MongoDB change streams | Kafka Connect CDC |
|---|---|---|---|---|
| Declared on the data model | Yes, `WITH EVENTS` | Publication/subscription outside table DDL | Watches are runtime API calls | Connector config outside DB schema |
| Built-in queue target | Yes, RedDB queues | No | No | Kafka topic |
| Producer-side redaction | Yes, `REDACT` | Requires plugin/filtering layer | Requires pipeline or app logic | Requires SMT/connector config |
| Tenant isolation | Engine-scoped tenant routing | Usually schema/database conventions | Database/collection and auth conventions | Topic naming and connector ACLs |
| Backpressure handling | Outbox plus `<queue>_outbox_dlq` | Slot lag; storage grows until consumed | Resume token and oplog retention window | Kafka offsets and connector retries |
| Query language integration | SQL/RQL DDL and queue commands | SQL plus replication protocol | Driver API | Kafka/connector API |
| Loop prevention | Queues cannot emit events | Trigger loops are user-managed | Watch loops are app-managed | Topic feedback loops are user-managed |
| Backfill bootstrap | `EVENTS BACKFILL` synthetic events | Snapshot/export plus replication slot | Initial query plus resume token | Snapshot mode in connector |

## Related RedDB Surfaces

- [Queues](queues.md) define `FANOUT`, `WORK`, consumer groups, ACK/NACK, and DLQ handling.
- [Query event syntax](../query/events.md) is the compact SQL/RQL command reference.
- [Policies](../security/policies.md#events-and-queue-policies) define source/target authorization and REDACT warnings.
- [red.* schema](../reference/red-schema.md#redsubscriptions) exposes subscription metadata through `red.subscriptions`.

## Conformance Corpus

These cases pin the documentation contract. Parser-backed cases live in
`crates/reddb-server/tests/conformance/`; runtime behavior is covered by
`tests/e2e_events_foundation.rs`.

```sql
-- [E-01] Auto queue
CREATE TABLE users (id INT, email TEXT, name TEXT) WITH EVENTS;
-- result: creates users and auto-creates users_events as FANOUT

-- [E-02] Custom queue
CREATE TABLE users (id INT, email TEXT) WITH EVENTS TO audit_log;
-- result: subscription targets audit_log

-- [E-03] Insert/update operation filter
CREATE TABLE users (id INT, email TEXT) WITH EVENTS (INSERT, UPDATE) TO audit_log;
-- result: delete events are suppressed

-- [E-04] Delete-only operation filter
CREATE TABLE users (id INT, email TEXT) WITH EVENTS (DELETE) TO audit_log;
-- result: only delete events are emitted

-- [E-05] REDACT one field
CREATE TABLE users (id INT, email TEXT) WITH EVENTS REDACT (email);
-- result: email is redacted in before/after

-- [E-06] REDACT multiple fields
CREATE TABLE accounts (id INT, email TEXT, phone TEXT, ssn TEXT) WITH EVENTS REDACT (email, phone, ssn);
-- result: all listed fields are redacted

-- [E-07] REDACT dotted JSON path
CREATE TABLE docs (id INT, body JSON) WITH EVENTS REDACT (body.user.email);
-- result: body.user.email is redacted

-- [E-08] REDACT wildcard JSON path
CREATE TABLE messages (id INT, body JSON) WITH EVENTS REDACT (body.*.email);
-- result: every direct child email under body is redacted

-- [E-09] WHERE filter
CREATE TABLE orders (id INT, status TEXT) WITH EVENTS WHERE status = 'paid';
-- result: only paid rows emit events

-- [E-10] Operation plus WHERE filter
CREATE TABLE orders (id INT, status TEXT) WITH EVENTS (INSERT, UPDATE) TO paid_orders WHERE status = 'paid';
-- result: only insert/update for paid rows emit

-- [E-11] Enable events on existing collection
ALTER TABLE users ENABLE EVENTS TO audit_log;
-- result: default subscription is enabled or re-enabled

-- [E-12] Enable delete-only events
ALTER TABLE users ENABLE EVENTS (DELETE) TO audit_log;
-- result: only delete events are emitted after enable

-- [E-13] Disable events
ALTER TABLE users DISABLE EVENTS;
-- result: subscription metadata remains, but no new events are emitted

-- [E-14] Add named subscription
ALTER TABLE orders ADD SUBSCRIPTION audit TO audit_log;
-- result: audit subscription is added

-- [E-15] Add named subscription with filter
ALTER TABLE orders ADD SUBSCRIPTION search TO search_sync WHERE status = 'paid';
-- result: search subscription only receives paid rows

-- [E-16] Add named subscription with REDACT
ALTER TABLE users ADD SUBSCRIPTION masked TO pii_events REDACT (email, phone);
-- result: only masked subscription redacts PII

-- [E-17] Drop named subscription
ALTER TABLE orders DROP SUBSCRIPTION search;
-- result: search subscription stops receiving new events

-- [E-18] Cross-tenant subscription
ALTER TABLE users ENABLE EVENTS TO global_audit ON ALL TENANTS;
-- result: requires cluster event-subscribe capability

-- [E-19] Cross-tenant subscription with capability marker
ALTER TABLE users ENABLE EVENTS TO global_audit ON ALL TENANTS REQUIRES CAPABILITY 'events:cluster_subscribe';
-- result: parser accepts explicit capability marker

-- [E-20] Loop prevention
CREATE QUEUE audit_log WITH EVENTS;
-- result: rejected; queues cannot have event subscriptions

-- [E-21] Insert event trigger
INSERT INTO users (id, email, name) VALUES (1, 'ada@example.com', 'Ada');
-- result: one insert event is enqueued for matching subscriptions

-- [E-22] Update event trigger
UPDATE users SET name = 'Ada Lovelace' WHERE id = 1;
-- result: one update event includes before and after

-- [E-23] Delete event trigger
DELETE FROM users WHERE id = 1;
-- result: one delete event includes before

-- [E-24] Truncate event trigger
TRUNCATE TABLE users;
-- result: one truncate event, not one delete event per row

-- [E-25] Drop event trigger
DROP TABLE users;
-- result: one collection_dropped event and queue remains drainable

-- [E-26] Queue group creation for consumers
QUEUE GROUP CREATE users_events sync_workers;
-- result: durable consumer group exists

-- [E-27] Queue read
QUEUE READ users_events GROUP sync_workers CONSUMER search_indexer COUNT 10;
-- result: consumer receives event payload messages

-- [E-28] Queue ack
QUEUE ACK users_events GROUP sync_workers 'msg-id-123';
-- result: processed event is acknowledged

-- [E-29] Queue nack
QUEUE NACK users_events GROUP sync_workers 'msg-id-123';
-- result: event becomes retryable according to queue policy

-- [E-30] DLQ inspection
QUEUE READ user_events_outbox_dlq GROUP ops CONSUMER dlq_inspector COUNT 10;
-- result: operator can inspect failed outbox payloads
```
