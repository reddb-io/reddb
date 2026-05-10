# Queues

RedDB includes a built-in message queue with FIFO/LIFO ordering, priority support, consumer groups, and dead-letter queues — no separate RabbitMQ or Redis required.

Every queue operates in one of two modes: **WORK** (task distribution — each message goes to one consumer) or **FANOUT** (broadcast — every consumer gets every message). Choose the mode at creation time; change it at runtime with `ALTER QUEUE SET MODE`.

Queues are also the delivery target for [Events](events.md). A collection
declared with `WITH EVENTS` emits mutation payloads into a queue; queues
themselves cannot emit events, which prevents subscription cycles.

## Quick Start

```sql
-- FANOUT: every subscriber receives every notification independently
CREATE QUEUE notifications FANOUT

-- WORK: each task is claimed by exactly one worker (default mode)
CREATE QUEUE tasks WORK
```

Read from a FANOUT queue — each consumer sees all messages:

```sql
QUEUE READ notifications CONSUMER app_server_1 COUNT 10
QUEUE READ notifications CONSUMER mobile_push  COUNT 10
```

Read from a WORK queue — messages are distributed across workers:

```sql
QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 5
QUEUE READ tasks GROUP workers CONSUMER worker2 COUNT 5
QUEUE ACK  tasks GROUP workers 'msg-id-123'
```

## Queue Modes

| Mode | Delivery | Internal group | Use case |
|------|----------|----------------|----------|
| **WORK** (default) | Each message → exactly one consumer | `_work_default` (shared) | Job queues, task distribution, ordered processing |
| **FANOUT** | Each message → all consumers independently | `_fanout_<consumer>` (per consumer) | Notifications, event broadcast, cache invalidation |

### WORK semantics

- All consumers share one implicit consumer group (`_work_default`).
- A message delivered to `worker1` is invisible to `worker2` until it times out or is NACK'd.
- Acknowledgment removes the message for all consumers.

### FANOUT semantics

- Each unique `CONSUMER` name gets its own implicit group (`_fanout_<name>`).
- A message acknowledged by `consumer_A` remains pending for `consumer_B`.
- Dead-letter quota is tracked per group — one slow consumer cannot block another.
- A consumer that never reads accumulates messages until TTL or DLQ limits apply.

## Comparison with Other Systems

| Feature | RedDB WORK | RedDB FANOUT | RabbitMQ | Apache Pulsar | Kafka |
|---------|-----------|-------------|----------|---------------|-------|
| Competing consumers | Yes (built-in) | No | Yes (round-robin) | Yes (shared subscription) | Yes (consumer group) |
| Broadcast to all | No | Yes (implicit) | Via fanout exchange | Via exclusive subscription | Via separate consumer groups |
| SQL interface | Yes | Yes | No | No | No |
| Schema-aware filtering | Yes (`WHERE`) | Yes (`WHERE`) | Limited (headers) | Limited | No |
| DLQ | Yes | Yes (per group) | Yes | Yes | No (manual) |
| Priority ordering | Yes (`PRIORITY`) | Yes (`PRIORITY`) | Yes | Yes | No |
| Mode change at runtime | `ALTER QUEUE SET MODE` | `ALTER QUEUE SET MODE` | No | No | No |

## Creating a Queue

```sql
-- WORK mode (default) — explicit or implicit
CREATE QUEUE tasks
CREATE QUEUE tasks WORK

-- FANOUT mode
CREATE QUEUE notifications FANOUT

-- Bounded queue (stops accepting when full)
CREATE QUEUE tasks MAX_SIZE 10000

-- With message TTL (messages expire after 24 h)
CREATE QUEUE events WITH TTL 24h

-- Priority queue (highest priority dequeued first)
CREATE QUEUE urgent_tasks PRIORITY

-- With dead-letter queue (after 3 failed attempts)
CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 3

-- Combined: bounded FANOUT queue with TTL
CREATE QUEUE updates FANOUT MAX_SIZE 50000 WITH TTL 1h
```

## Changing Mode at Runtime

```sql
ALTER QUEUE notifications SET MODE WORK
ALTER QUEUE tasks SET MODE FANOUT
```

**In-flight semantics**: messages already pending (delivered but not yet acknowledged) drain with the **old** mode. New reads after the `ALTER` use the **new** mode. The engine emits a warning in the audit log when pending messages exist at switch time.

After switching a FANOUT queue to WORK, per-consumer groups (`_fanout_*`) remain in storage but new reads route through `_work_default`. After switching WORK to FANOUT, the existing `_work_default` group stops receiving new reads; new consumer names get `_fanout_<name>` groups.

## Push / Pop

### Push (enqueue)

```sql
-- Push to back (FIFO enqueue, default)
QUEUE PUSH tasks {"job":"process","id":123}
QUEUE RPUSH tasks {"job":"process","id":456}

-- Push to front (deque / urgent)
QUEUE LPUSH tasks {"urgent":true}

-- Push with priority (priority queues only)
QUEUE PUSH urgent_tasks {"job":"deploy"} PRIORITY 10
```

### Pop (dequeue)

```sql
-- Pop from front (FIFO, default)
QUEUE POP tasks
QUEUE LPOP tasks

-- Pop from back (LIFO / stack)
QUEUE RPOP tasks

-- Pop multiple
QUEUE POP tasks COUNT 5
```

### Peek (read without removing)

```sql
QUEUE PEEK tasks
QUEUE PEEK tasks 5
```

### Queue metadata

```sql
QUEUE LEN tasks        -- message count
TRUNCATE QUEUE tasks   -- remove all messages (canonical DDL)
QUEUE PURGE tasks      -- backward-compatible alias for TRUNCATE QUEUE
```

## Consumer Groups

Consumer groups give explicit, named tracking of delivery and acknowledgment state. In WORK mode you usually rely on the implicit `_work_default` group; explicit groups are useful when multiple independent application tiers each need to consume the entire queue independently (functionally equivalent to FANOUT but with granular group names).

### Setup

```sql
CREATE QUEUE orders WORK
QUEUE GROUP CREATE orders billing
QUEUE GROUP CREATE orders fulfillment
```

### Read as a consumer

```sql
QUEUE READ orders GROUP billing     CONSUMER invoice-svc COUNT 10
QUEUE READ orders GROUP fulfillment CONSUMER warehouse-1 COUNT 10
```

Messages remain pending until acknowledged. Unacknowledged messages become available again after an idle timeout or a worker crash.

### Acknowledgment

```sql
-- Successful processing
QUEUE ACK  orders GROUP billing 'msg-id-abc'

-- Failed — requeue for retry
QUEUE NACK orders GROUP billing 'msg-id-abc'
```

### Pending messages

```sql
QUEUE PENDING orders GROUP billing
QUEUE CLAIM   orders GROUP billing CONSUMER invoice-svc-2 MIN_IDLE 60000
```

## Priority Queues

```sql
CREATE QUEUE alerts PRIORITY

QUEUE PUSH alerts {"level":"info"}     PRIORITY 1
QUEUE PUSH alerts {"level":"warning"}  PRIORITY 5
QUEUE PUSH alerts {"level":"critical"} PRIORITY 10

-- Returns critical → warning → info
QUEUE POP alerts
QUEUE POP alerts
QUEUE POP alerts
```

## Dead-Letter Queue

Messages exceeding `MAX_ATTEMPTS` are automatically moved to the DLQ:

```sql
CREATE QUEUE jobs WITH DLQ failed_jobs MAX_ATTEMPTS 3
```

In FANOUT mode, the DLQ threshold is tracked per consumer group. A message may be DLQ'd for one consumer while still live for another.

### Inspect and replay DLQ messages

Use `SELECT ... FROM QUEUE` for read-only queue inspection. It does not consume, lease, ACK, NACK, or mutate consumer-group state.

```sql
SELECT id, payload, attempts, last_error, enqueued_at
FROM QUEUE failed_jobs
WHERE attempts >= 3
LIMIT 50
```

Queue projection columns are `id`, `payload`, `priority`, `attempts`, `last_error`, `enqueued_at`, `available_at`, `dlq`, and `tenant`.

Use `QUEUE MOVE` to replay a bounded batch from one queue to another:

```sql
QUEUE MOVE FROM failed_jobs TO jobs
WHERE attempts >= 3
LIMIT 100
```

`QUEUE MOVE` snapshots eligible source messages, applies the optional predicate, then removes from the source and appends to the destination as one replay operation. If the destination cannot accept the selected batch, the source remains unchanged. A `WHERE` clause requires an explicit `LIMIT`; without `WHERE`, the default limit is one message. Each committed move emits a `queue/move` audit event with source, destination, selected count, and committed count.

## Delivery Guarantees

- **At-least-once delivery**: unacknowledged messages reappear after crash recovery.
- **Consumer group tracking**: per-consumer delivery state with NACK/claim support.
- **WAL integration**: push/pop/ack are durable via write-ahead log.

## Introspection

```sql
-- List all queues with their mode
SHOW QUEUES

-- Full metadata including queue_mode column
SELECT name, queue_mode, entities FROM red.collections WHERE model = 'queue'
```

The `queue_mode` column in `red.collections` reports `fanout` or `work` for queue collections and `NULL` for all other models. See [`docs/reference/red-schema.md`](../reference/red-schema.md#redcollections) for the full `red.collections` schema.

## Conformance Corpus

The cases below document expected parser and runtime behaviour.

### CREATE — basic forms

```sql
-- [C-01] Default mode is WORK
CREATE QUEUE jobs
-- result: queue 'jobs' created, mode = work

-- [C-02] Explicit WORK
CREATE QUEUE jobs WORK
-- result: queue 'jobs' created, mode = work

-- [C-03] Explicit FANOUT
CREATE QUEUE notifications FANOUT
-- result: queue 'notifications' created, mode = fanout

-- [C-04] FANOUT with MAX_SIZE
CREATE QUEUE events FANOUT MAX_SIZE 5000
-- result: queue 'events' created, mode = fanout, max_size = 5000

-- [C-05] WORK with DLQ and max attempts
CREATE QUEUE tasks WORK WITH DLQ dead_tasks MAX_ATTEMPTS 5
-- result: queue 'tasks' created, mode = work, dlq = 'dead_tasks', max_attempts = 5
```

### ALTER — mode change

```sql
-- [C-06] WORK → FANOUT
CREATE QUEUE q WORK
ALTER QUEUE q SET MODE FANOUT
-- result: subsequent reads use per-consumer fanout groups
-- audit log: no warning (no in-flight messages)

-- [C-07] FANOUT → WORK with in-flight messages
-- setup: CREATE QUEUE q FANOUT; QUEUE PUSH q '{"x":1}'; QUEUE READ q CONSUMER c1 COUNT 1
ALTER QUEUE q SET MODE WORK
-- result: mode switched; audit log WARN "1 in-flight messages will drain with old mode"
-- after drain: new reads via _work_default

-- [C-08] ALTER on non-existent queue
ALTER QUEUE no_such_queue SET MODE FANOUT
-- result: error "queue 'no_such_queue' not found"
```

### SHOW QUEUES / red.collections

```sql
-- [C-09] queue_mode reflected in red.collections
CREATE QUEUE n FANOUT
SELECT queue_mode FROM red.collections WHERE name = 'n'
-- result: "fanout"

-- [C-10] SHOW QUEUES lists both modes
CREATE QUEUE a WORK
CREATE QUEUE b FANOUT
SHOW QUEUES
-- result: rows for a (work) and b (fanout) present
```

### Edge cases

```sql
-- [C-11] Duplicate CREATE
CREATE QUEUE tasks
CREATE QUEUE tasks
-- result: second statement errors "already exists" (no IF NOT EXISTS)

-- [C-12] FANOUT: independent delivery per consumer
CREATE QUEUE n FANOUT
QUEUE PUSH n '{"msg":1}'
QUEUE READ n CONSUMER c1 COUNT 1  -- returns msg
QUEUE READ n CONSUMER c2 COUNT 1  -- also returns msg (independent)

-- [C-13] WORK: single delivery
CREATE QUEUE t WORK
QUEUE PUSH t '{"job":1}'
QUEUE READ t GROUP workers CONSUMER w1 COUNT 1  -- returns job
QUEUE READ t GROUP workers CONSUMER w2 COUNT 1  -- returns nothing (already claimed)

-- [C-14] Mode persists across restart (stored in queue meta)
CREATE QUEUE persistent FANOUT
-- restart server
SELECT queue_mode FROM red.collections WHERE name = 'persistent'
-- result: "fanout"
```

## See Also

- [Key-Value](key-value.md) — simple key-value storage
- [Time-Series](timeseries.md) — time-stamped metric data
- [`red.collections` schema reference](../reference/red-schema.md#redcollections)
