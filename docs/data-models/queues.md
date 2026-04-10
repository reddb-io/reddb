# Queues & Deques

RedDB includes a built-in message queue with FIFO/LIFO ordering, priority support, consumer groups, and dead-letter queues. No need for a separate RabbitMQ or Redis queue.

## When to Use

- Task/job queues
- Event-driven architectures
- Inter-service messaging
- Rate limiting with bounded queues
- Reliable message delivery with acknowledgment

## Creating a Queue

```sql
-- Simple FIFO queue
CREATE QUEUE tasks

-- Bounded queue
CREATE QUEUE tasks MAX_SIZE 10000

-- With message TTL
CREATE QUEUE tasks WITH TTL 24 h

-- Priority queue (highest priority dequeued first)
CREATE QUEUE urgent_tasks PRIORITY

-- With dead-letter queue
CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 3
```

## Push / Pop

### Push (Enqueue)

```sql
-- Push to back (default, FIFO enqueue)
QUEUE PUSH tasks '{"job":"process","id":123}'
QUEUE RPUSH tasks '{"job":"process","id":456}'

-- Push to front (deque operation)
QUEUE LPUSH tasks '{"urgent":true}'

-- Push with priority (priority queues only)
QUEUE PUSH urgent_tasks '{"job":"deploy"}' PRIORITY 10
```

### Pop (Dequeue)

```sql
-- Pop from front (FIFO dequeue)
QUEUE POP tasks
QUEUE LPOP tasks

-- Pop from back (LIFO / stack behavior)
QUEUE RPOP tasks

-- Pop multiple messages
QUEUE POP tasks COUNT 5
```

### Peek (Read Without Removing)

```sql
-- Peek at next message
QUEUE PEEK tasks

-- Peek at next 5 messages
QUEUE PEEK tasks 5
```

### Queue Info

```sql
-- Get queue length
QUEUE LEN tasks

-- Remove all messages
QUEUE PURGE tasks
```

## Consumer Groups

Multiple consumers can read from the same queue independently. Each consumer group tracks delivery and acknowledgment.

### Setup

```sql
-- Create a consumer group
QUEUE GROUP CREATE tasks workers
```

### Reading as a Consumer

```sql
-- Read up to 5 messages as consumer "worker1"
QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 5
```

Messages remain pending until acknowledged. If a consumer crashes, unacknowledged messages become available again.

### Acknowledgment

```sql
-- Acknowledge successful processing
QUEUE ACK tasks GROUP workers 'message_id'

-- Negative acknowledge (requeue for retry)
QUEUE NACK tasks GROUP workers 'message_id'
```

### Pending Messages

```sql
-- List pending messages for a consumer group
QUEUE PENDING tasks GROUP workers

-- Claim pending messages from idle consumers
QUEUE CLAIM tasks GROUP workers CONSUMER worker2 MIN_IDLE 60000
```

## Priority Queues

When created with `PRIORITY`, the queue dequeues highest-priority messages first:

```sql
CREATE QUEUE alerts PRIORITY

QUEUE PUSH alerts '{"level":"info"}' PRIORITY 1
QUEUE PUSH alerts '{"level":"critical"}' PRIORITY 10
QUEUE PUSH alerts '{"level":"warning"}' PRIORITY 5

-- Returns: critical (10), then warning (5), then info (1)
QUEUE POP alerts
QUEUE POP alerts
QUEUE POP alerts
```

## Dead-Letter Queue

Messages that exceed `MAX_ATTEMPTS` are automatically moved to the dead-letter queue:

```sql
CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 3
```

Failed messages can be inspected and reprocessed from the DLQ.

## Delivery Guarantees

- **At-least-once delivery**: Unacknowledged messages reappear after crash recovery
- **Consumer groups**: Track per-consumer delivery state
- **WAL integration**: Push/pop/ack operations are durable via write-ahead log

## See Also

- [Key-Value](/data-models/key-value.md) -- Simple key-value storage
- [Time-Series](/data-models/timeseries.md) -- Time-stamped metric data
