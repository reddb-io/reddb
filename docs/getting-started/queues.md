# Queues Quickstart

Use this when work needs to be claimed, retried, or consumed in order. The
Collection is the universal container; the queue model is the semantic layer.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
CREATE QUEUE jobs PRIORITY;
QUEUE PUSH jobs {task: 'send_email', id: 42} PRIORITY 10;
QUEUE PEEK jobs COUNT 1;
QUEUE POP jobs COUNT 1;
```

First meaningful result: the peek shows the next message and the pop claims it.

Where to go next: [Queues & Deques](/data-models/queues.md) and
[Transactions & MVCC](/query/transactions.md#queues-and-transactions).
