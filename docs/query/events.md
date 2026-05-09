# Event Subscriptions

`WITH EVENTS` declares table-to-queue event subscription metadata. This foundation persists the subscription in the collection catalog and creates the target queue when it is missing. It does not emit row-change events yet.

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

Queues cannot subscribe to events themselves:

```sql
CREATE QUEUE audit_log WITH EVENTS;
```

That statement is rejected with `queues cannot have event subscriptions`. RedDB also rejects subscription graphs that would create a cycle.
