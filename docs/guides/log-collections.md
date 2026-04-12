# Log Collections

RedDB provides high-performance append-only log collections optimized for sequential writes, time-ordered queries, and automatic retention. Log entries use timestamp-based IDs that carry temporal information, eliminating the need for separate timestamp columns.

---

## The ID is the Timestamp

Every log entry receives an auto-generated `id` that encodes the insertion time. Sorting by `id` is sorting by time. Querying by `id` range is querying by time range. There is no separate timestamp column to manage.

### ID Layout (64 bits)

```
┌──────────────────────────────────┬──────────┐
│ timestamp_us (52 bits)           │ seq (12) │
│ microsecond precision            │ 4096/µs  │
└──────────────────────────────────┴──────────┘
```

| Component | Bits | Range | Purpose |
|:----------|:-----|:------|:--------|
| `timestamp_us` | 52 | ~142 years from epoch | Microsecond-precision insertion time |
| `seq` | 12 | 0 - 4095 | Uniqueness within the same microsecond |

**Properties:**
- Monotonically increasing (no out-of-order IDs, even under concurrency)
- Collision-free up to 4,095 entries per microsecond (~4 billion entries/sec theoretical)
- Time-extractable: `timestamp_ms = id >> 12 / 1000`
- Naturally sorted: `ORDER BY id` = `ORDER BY time`
- Thread-safe: atomic CAS generator, no locks

### Extracting Time from an ID

The `id` is a plain integer that you can decompose:

```
id = 7352947238912

timestamp_us = id >> 12        = 1795152646
timestamp_ms = id >> 12 / 1000 = 1795152
timestamp_s  = id >> 12 / 1000000 = 1795
```

In practice, use the `timestamp_ms` field returned by the API — it does this extraction for you.

---

## Schema

Every log collection has one system-managed column and any number of user-defined columns:

```
┌──────────┬──────────┬─────────────────────────────┐
│ column   │ type     │ note                        │
├──────────┼──────────┼─────────────────────────────┤
│ id       │ INTEGER  │ auto (timestamp_us + seq)   │
│ level    │ TEXT     │ user-defined                │
│ message  │ TEXT     │ user-defined                │
│ trace_id │ TEXT     │ user-defined                │
│ ...      │ ...      │ any fields you want         │
└──────────┴──────────┴─────────────────────────────┘
```

The `id` column is the only system-managed field. It appears in every query result, is always populated by the system, and serves as both the primary key and the time index. There are no hidden or magic underscore fields.

---

## HTTP API

### POST /logs/{name}/append

Append a single log entry. Returns immediately (write-buffered for throughput).

```bash
curl -X POST localhost:8080/logs/app_logs/append -d '{
  "level": "info",
  "message": "request handled",
  "path": "/api/users",
  "latency_ms": 42,
  "trace_id": "abc-123-def"
}'
```

**Response:**
```json
{
  "ok": true,
  "id": 7352947238912,
  "timestamp_ms": 1712880000000
}
```

The `id` is the log entry's unique identifier and its timestamp combined. The `timestamp_ms` is a convenience extraction in milliseconds since epoch.

You can send any JSON object — all fields become columns on the entry.

### GET /logs/{name}/query

Query log entries. Returns newest-first by default.

```bash
# Last 100 entries (newest first)
curl "localhost:8080/logs/app_logs/query?limit=100"

# Entries since a specific ID (for pagination / tailing)
curl "localhost:8080/logs/app_logs/query?since=7352947238912&limit=50"
```

**Response:**
```json
{
  "ok": true,
  "count": 3,
  "entries": [
    {
      "id": 7352947240000,
      "timestamp_ms": 1712880000001,
      "level": "error",
      "message": "connection refused",
      "trace_id": "xyz-456"
    },
    {
      "id": 7352947238912,
      "timestamp_ms": 1712880000000,
      "level": "info",
      "message": "request handled",
      "path": "/api/users",
      "latency_ms": 42
    }
  ]
}
```

**Query parameters:**

| Parameter | Type | Default | Description |
|:----------|:-----|:--------|:------------|
| `limit` | integer | 100 | Maximum entries to return |
| `since` | integer (log ID) | none | Return entries after this ID (exclusive) |

The `since` parameter enables efficient log tailing: store the last `id` you received, then poll with `since=<last_id>` to get only new entries.

### POST /logs/{name}/retention

Manually trigger retention cleanup for a log collection.

```bash
curl -X POST localhost:8080/logs/app_logs/retention
```

**Response:**
```json
{
  "ok": true,
  "deleted": 1500,
  "remaining": 8500
}
```

---

## Retention Policies

Log collections support three retention strategies:

| Policy | Behavior | Example |
|:-------|:---------|:--------|
| **Days** | Delete entries older than N days | `LogRetention::Days(7)` — keep last week |
| **MaxEntries** | Keep at most N entries, evict oldest | `LogRetention::MaxEntries(100000)` — cap at 100K |
| **Forever** | Never auto-delete (default) | Manual cleanup only |

Retention is applied:
- Manually via `POST /logs/{name}/retention`
- Automatically in the background maintenance thread (if configured)

---

## Embedded (Rust API)

```rust
use reddb::log::{LogCollection, LogCollectionConfig, LogRetention};
use reddb::storage::schema::Value;

let db = RedDB::open("./data.rdb")?;
let store = db.store();

// Create a log collection with 7-day retention
let mut config = LogCollectionConfig::new("app_logs");
config.retention = LogRetention::Days(7);
config.batch_size = 128; // buffer 128 entries before flushing

let log = LogCollection::new(store, config);

// Append entries (returns immediately, buffered)
let id = log.append_fields(vec![
    ("level", Value::Text("info".into())),
    ("message", Value::Text("server started".into())),
    ("port", Value::Integer(8080)),
]);

// The id carries the timestamp
println!("Logged at: {}ms", id.timestamp_ms());

// Query recent entries
let entries = log.recent(50);
for entry in &entries {
    println!("[{}] {}: {}",
        entry.id.timestamp_ms(),
        entry.fields.get("level").unwrap_or(&Value::Null),
        entry.fields.get("message").unwrap_or(&Value::Null),
    );
}

// Range query by time (using ID boundaries)
use reddb::log::LogId;
let one_hour_ago = LogId::from_ms(
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        - 3_600_000
);
let now = LogId::from_ms(u64::MAX / 4096); // far future
let recent_hour = log.range(one_hour_ago, now, 1000);

// Apply retention
let deleted = log.apply_retention();
println!("Cleaned up {} old entries", deleted);
```

---

## Write Buffering

Log entries are buffered in memory before being flushed to storage. This amortizes the cost of individual writes across a batch.

| Setting | Default | Description |
|:--------|:--------|:------------|
| `batch_size` | 64 | Number of entries buffered before auto-flush |

The buffer flushes when:
- `batch_size` entries accumulate (automatic)
- `recent()` or `range()` is called (ensures read consistency)
- The `LogCollection` is dropped (Rust Drop impl)
- `flush_buffer()` is called explicitly

For maximum throughput, use a larger `batch_size` (e.g., 256 or 1024) and accept slightly delayed visibility. For real-time tailing, use `batch_size = 1`.

---

## Log Tailing Pattern

To continuously tail a log collection from an HTTP client:

```bash
# Initial fetch
LAST_ID=0

while true; do
  RESPONSE=$(curl -s "localhost:8080/logs/app_logs/query?since=$LAST_ID&limit=100")
  
  # Process entries
  echo "$RESPONSE" | jq -r '.entries[] | "\(.timestamp_ms) [\(.level)] \(.message)"'
  
  # Update cursor
  NEW_LAST=$(echo "$RESPONSE" | jq -r '.entries[0].id // empty')
  if [ -n "$NEW_LAST" ]; then
    LAST_ID=$NEW_LAST
  fi
  
  sleep 1
done
```

This pattern is efficient because `since` uses the ID's natural ordering — the database doesn't need to scan old entries.

---

## Comparison with Regular Tables

| Feature | Regular Table | Log Collection |
|:--------|:-------------|:---------------|
| ID generation | Per-table `row_id` (sequential) | Timestamp-based (carries time info) |
| Write pattern | Random insert/update/delete | Append-only (no update/delete by user) |
| Write buffering | Immediate | Batched (configurable) |
| Primary use case | CRUD entities | Event streams, audit trails, metrics |
| Retention | Manual or TTL metadata | Built-in (Days, MaxEntries, Forever) |
| Time queries | Requires timestamp column + index | Native (ORDER BY id = ORDER BY time) |
| Schema | User-defined | User-defined + system `id` column |

Log collections are built on top of the same UnifiedStore as regular tables. They benefit from the same persistence, replication, and backup infrastructure. The difference is in the write path (buffered, append-only) and the ID semantics (timestamp-encoded).

---

## Example: Application Logging

```bash
# Structured application logs
curl -X POST localhost:8080/logs/app/append -d '{
  "level": "info",
  "service": "api-gateway",
  "method": "GET",
  "path": "/users/42",
  "status": 200,
  "latency_ms": 12,
  "trace_id": "tr-abc-123"
}'

# Error with stack trace
curl -X POST localhost:8080/logs/app/append -d '{
  "level": "error",
  "service": "auth-service",
  "message": "token validation failed",
  "error": "ExpiredSignatureError",
  "user_id": 42
}'

# Query errors from the last hour
curl "localhost:8080/logs/app/query?limit=50"
```

## Example: Audit Trail

```bash
# Record every data change
curl -X POST localhost:8080/logs/audit/append -d '{
  "actor": "user:admin",
  "action": "update",
  "resource": "users/42",
  "changes": {"role": "admin"},
  "ip": "10.0.1.5"
}'

# Query audit trail for a specific resource
curl "localhost:8080/logs/audit/query?limit=100"
```

## Example: Metrics Collection

```bash
# High-frequency metric ingestion
for i in $(seq 1 1000); do
  curl -s -X POST localhost:8080/logs/metrics/append -d "{
    \"host\": \"srv-1\",
    \"metric\": \"cpu.idle\",
    \"value\": $(( RANDOM % 100 ))
  }" &
done
wait

# Query recent metrics
curl "localhost:8080/logs/metrics/query?limit=10"
```
