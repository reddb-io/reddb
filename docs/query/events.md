# Event Subscriptions

`WITH EVENTS` declares collection-to-queue event subscriptions. The canonical
data-model guide is [Events](../data-models/events.md); this page is the compact
SQL/RQL syntax reference.

```sql
CREATE TABLE users (id INT, email TEXT) WITH EVENTS;
```

When `TO` is omitted, RedDB creates `<table>_events` as a fanout queue.

```sql
CREATE TABLE users (id INT, email TEXT) WITH EVENTS TO audit_log;
CREATE TABLE users (id INT, email TEXT) WITH EVENTS (INSERT, UPDATE);
CREATE TABLE users (id INT, email TEXT) WITH EVENTS REDACT (email);
CREATE TABLE users (id INT, email TEXT, status TEXT) WITH EVENTS WHERE status = 'active';
```

Subscriptions can be changed with `ALTER TABLE`:

```sql
ALTER TABLE users ENABLE EVENTS TO audit_log;
ALTER TABLE users DISABLE EVENTS;
```

Existing rows are not emitted automatically when a subscription is created.
Use `EVENTS BACKFILL` to enqueue synthetic bootstrap events for a subscription
target:

```sql
EVENTS BACKFILL users TO audit_log;
EVENTS BACKFILL users WHERE status = 'active' TO audit_log LIMIT 1000;
```

Backfill events carry `synthetic: true`, use deterministic event ids so reruns
do not duplicate queue messages, respect the target subscription's `REDACT`
clause, and follow the active tenant scope. `EVENTS BACKFILL STATUS <collection>`
is reserved for the status slice and currently returns an explicit
not-implemented error.

Inspect subscriptions with `EVENTS STATUS`:

```sql
EVENTS STATUS;
EVENTS STATUS users;
SELECT * FROM red.subscriptions;
```

Queues cannot subscribe to events themselves:

```sql
CREATE QUEUE audit_log WITH EVENTS;
```

That statement is rejected with `queues cannot have event subscriptions`. RedDB also rejects subscription graphs that would create a cycle.
