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
 payload | consumer | delivery_id
---------+----------+-------------------
 job-1   | worker1  | <opaque-delivery>
```

Acknowledge the message with
`QUEUE ACK tasks WITH delivery_id = '<opaque-delivery>'` once the work is done,
or it will be redelivered. Treat `delivery_id` as opaque and return it unchanged.

## 5. Run an application worker

Official SDKs provide a long-running worker that performs the server-side
wait, invokes the handler, ACKs success, and NACKs failure. JavaScript example:

```javascript
const worker = db.queue('tasks').consumer({
  consumer: 'worker-1',
  group: 'workers',
  waitMs: 30_000,
  retryDelay: '5s',
  async handleMessage(message) {
    await processJob(message.payload)
  },
})

worker.start()

process.once('SIGTERM', async () => {
  await worker.stop()
  await db.close()
})
```

`QUEUE READ ... WAIT` is a server-side wait, not interval polling. The worker
is sequential by default and only reads the next delivery after ACK/NACK. Run
one worker on a dedicated connection in each pod; two pods already give two
independent consumers. Handlers must be idempotent because delivery is at
least once.

## Where to go next

- [Queues & Deques](/data-models/queues.md) — the full queue model
- [Application clients & queue workers](/clients/application-clients.md) — all SDK APIs and lifecycle guarantees
- [Events](/data-models/events.md) — pub/sub over the same engine
- [Event Workflow](/data-models/event-workflow.md) — multi-stage processing
