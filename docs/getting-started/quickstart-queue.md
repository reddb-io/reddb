# Quickstart: Queues

Hand work to background consumers with at-least-once delivery. The **queue**
model is a semantic layer over a `collection` (the universal container):
messages, consumer groups, and a dead-letter queue for poison messages.

## 1. Start RedDB

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

Connect with `red connect 127.0.0.1:55055` (or POST to
`http://127.0.0.1:5000/query`).

## 2. Create a queue and a consumer group

Failed messages retry up to three times before landing in `failed_tasks`:

```sql
CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 3;
QUEUE GROUP CREATE tasks workers;
```

## 3. Push work

```sql
QUEUE PUSH tasks 'job-1';
QUEUE PUSH tasks 'job-2';
```

## 4. Your first meaningful result

Check the backlog, then deliver the first job to a consumer:

```sql
QUEUE LEN tasks;
QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 1;
```

```text
 payload | consumer
---------+---------
 job-1   | worker1
```

Acknowledge the message with `QUEUE ACK tasks GROUP workers '<message_id>'`
once the work is done, or it will be redelivered.

## Where to go next

- [Queues & Deques](/data-models/queues.md) — the full queue model
- [Events](/data-models/events.md) — pub/sub over the same engine
- [Event Workflow](/data-models/event-workflow.md) — multi-stage processing
