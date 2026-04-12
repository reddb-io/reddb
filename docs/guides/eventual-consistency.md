# Eventual Consistency

RedDB implements an eventual consistency model inspired by the transaction-log-and-consolidation pattern. This guide covers the theory behind it, how RedDB implements it, and how to use it across all deployment modes.

---

## The Theory

### What is eventual consistency?

In a strongly consistent system, a write is immediately visible to all subsequent reads. This is simple to reason about but limits throughput: every write must synchronize before returning.

Eventual consistency relaxes this constraint. A write returns immediately, and the system guarantees that all replicas will **eventually** converge to the same value, given enough time and no new writes. The trade-off is a window during which reads may return stale data.

### Why use it?

Strong consistency requires coordination (locks, consensus, two-phase commits) that adds latency and reduces throughput. Eventual consistency is the right choice when:

- **High write throughput** matters more than read-after-write consistency (counters, analytics, click tracking)
- **Availability** is more important than immediate accuracy (shopping carts, recommendation scores)
- **Conflict-free merging** is possible (numeric accumulations, last-write-wins timestamps)

### The CAP theorem

The CAP theorem states that a distributed system can provide at most two of three guarantees simultaneously: **Consistency**, **Availability**, and **Partition tolerance**. Since network partitions are inevitable, the real choice is between consistency (CP) and availability (AP). Eventual consistency is an AP strategy: the system stays available during partitions and converges after they heal.

### CRDTs and convergence

Conflict-Free Replicated Data Types (CRDTs) are data structures that mathematically guarantee convergence without coordination. A **G-Counter** (grow-only counter) is the simplest CRDT: each node tracks its own count, and the merged value is the sum across all nodes. RedDB's EC reducers follow the same principle. A `Sum` reducer is semantically a G-Counter. A `Max` reducer is a lattice join. These operations are commutative and associative, so they converge regardless of message ordering.

### Transaction log + consolidation

RedDB's EC model is based on an **append-only transaction log** with **periodic consolidation**:

```
Write path:   mutation → append to transaction log → return immediately
Read path:    read consolidated value (may be stale)
Background:   periodically consolidate pending transactions into the target field
```

This is similar to how event sourcing works. The transaction log is the source of truth. The consolidated field value is a materialized view that gets rebuilt from the log. Consolidation is idempotent: running it twice produces the same result.

---

## How RedDB Implements It

### Architecture

```
┌────────────────────────────────────────────────────────────────┐
│  Client: ec_add("wallets", "balance", id, 100.0)               │
└──────────────────────────┬─────────────────────────────────────┘
                           │
                           ▼
┌────────────────────────────────────────────────────────────────┐
│  Transaction Log (red_ec_tx_wallets_balance collection)            │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │ tx-1: target=42, op=Add, value=100, ts=1001, applied=F  │  │
│  │ tx-2: target=42, op=Add, value=50,  ts=1002, applied=F  │  │
│  │ tx-3: target=42, op=Sub, value=20,  ts=1003, applied=F  │  │
│  └──────────────────────────────────────────────────────────┘  │
└──────────────────────────┬─────────────────────────────────────┘
                           │
                    consolidation
                           │
                           ▼
┌────────────────────────────────────────────────────────────────┐
│  Target Record (wallets collection, entity 42)                  │
│  balance: 0 + 100 + 50 - 20 = 130                              │
│                                                                 │
│  Transaction Log (post-consolidation):                          │
│  │ tx-1: applied=T  │  tx-2: applied=T  │  tx-3: applied=T  │  │
└────────────────────────────────────────────────────────────────┘
```

### Transaction log

Every EC mutation creates an immutable transaction record stored in an internal collection named `red_ec_tx_{collection}_{field}`. Each transaction contains:

| Field | Type | Description |
|:------|:-----|:------------|
| `target_id` | u64 | Entity ID of the target record |
| `field` | String | Field name being modified |
| `value` | f64 | Amount to add, subtract, or set |
| `operation` | Add / Sub / Set | Type of mutation |
| `timestamp` | u64 | Unix milliseconds when created |
| `cohort_hour` | String | Time bucket for efficient querying |
| `applied` | bool | Whether this transaction has been consolidated |
| `source` | String? | Optional metadata (origin, user, etc.) |

Transactions are never modified after creation (except the `applied` flag). This makes the log an append-only audit trail.

### Consolidation algorithm

1. Query all transactions where `applied = false` for the target field
2. Group by `target_id`
3. For each group, sort by `timestamp`
4. Find the last `Set` operation (if any) — this becomes the base value
5. Apply all `Add`/`Sub` operations after the last `Set` using the configured reducer
6. Write the consolidated value to the target entity's field
7. Mark all processed transactions as `applied = true`

### Reducers

A reducer defines how values are combined during consolidation.

| Reducer | Formula | Use case |
|:--------|:--------|:---------|
| `Sum` | `current + incoming` | Counters, balances, scores |
| `Max` | `max(current, incoming)` | High scores, peak values |
| `Min` | `min(current, incoming)` | Minimum bids, lowest latency |
| `Count` | `current + 1` | Event counting (ignores value) |
| `Average` | `(current * n + incoming) / (n + 1)` | Running averages |
| `Last` | `incoming` | Last-write-wins timestamps |

The `Sum` reducer is the default and the most common. It's mathematically equivalent to a G-Counter CRDT when all operations are `Add`.

### Sync vs Async mode

| Mode | Behavior | Latency | Consistency |
|:-----|:---------|:--------|:------------|
| **Sync** | Consolidates immediately after each mutation | Higher (write + consolidate) | Strong (read-after-write) |
| **Async** | Queues transaction, background worker consolidates later | Lower (write only) | Eventual (stale reads possible) |

Sync mode is appropriate for financial operations where you need to read the updated balance immediately. Async mode is appropriate for analytics, counters, and high-throughput scenarios where eventual convergence is acceptable.

---

## Configuration

### Via red_config (declarative)

```sql
-- Register fields for eventual consistency
SET CONFIG 'red.ec.wallets.fields' = '["balance", "points"]'
SET CONFIG 'red.ec.wallets.balance.reducer' = 'sum'
SET CONFIG 'red.ec.wallets.balance.mode' = 'async'
SET CONFIG 'red.ec.wallets.balance.interval_secs' = '60'
SET CONFIG 'red.ec.wallets.points.reducer' = 'sum'
SET CONFIG 'red.ec.wallets.points.mode' = 'sync'
```

The EC registry loads automatically from `red_config` at startup.

### Via Rust API (embedded)

```rust
use reddb::ec::config::{EcFieldConfig, EcReducer, EcMode};

let db = RedDB::open("./data.rdb")?;

let mut config = EcFieldConfig::new("wallets", "balance");
config.reducer = EcReducer::Sum;
config.mode = EcMode::Async;
config.consolidation_interval_secs = 30;
db.ec_register(config);
```

---

## Usage

### HTTP API

```bash
# Add 100 to wallet balance
curl -X POST localhost:8080/ec/wallets/balance/add \
  -d '{"id": 42, "value": 100}'

# Subtract 20
curl -X POST localhost:8080/ec/wallets/balance/sub \
  -d '{"id": 42, "value": 20}'

# Set to exact value (overrides previous add/sub)
curl -X POST localhost:8080/ec/wallets/balance/set \
  -d '{"id": 42, "value": 500}'

# Check status (consolidated + pending)
curl localhost:8080/ec/wallets/balance/status?id=42

# Manually trigger consolidation
curl -X POST localhost:8080/ec/wallets/balance/consolidate

# Global EC status
curl localhost:8080/ec/status
```

**Status response:**
```json
{
  "ok": true,
  "consolidated": 130.0,
  "pending_value": 0.0,
  "pending_transactions": 0,
  "has_pending_set": false,
  "field": "balance",
  "collection": "wallets",
  "reducer": "sum",
  "mode": "async"
}
```

### Embedded (Rust)

```rust
let db = RedDB::open("./data.rdb")?;

// Create a wallet
let id = db.row("wallets", vec![
    ("name", Value::Text("Alice".into())),
    ("balance", Value::Float(0.0)),
]).save()?;

// EC operations
db.ec_add("wallets", "balance", id, 100.0)?;
db.ec_add("wallets", "balance", id, 50.0)?;
db.ec_sub("wallets", "balance", id, 20.0)?;

// Check status before consolidation
let status = db.ec_status("wallets", "balance", id.raw());
assert_eq!(status.pending_transactions, 3);

// Consolidate
db.ec_consolidate("wallets", "balance", Some(id.raw()))?;

// Now the balance field is updated
let status = db.ec_status("wallets", "balance", id.raw());
assert_eq!(status.consolidated, 130.0);
assert_eq!(status.pending_transactions, 0);

// flush() auto-consolidates all EC fields before persisting
db.flush()?;
```

### Serverless

In serverless mode, the EC background worker runs during the warm phase. On reclaim, include `"ec_consolidate"` in the operations list to ensure all pending transactions are consolidated before shutdown:

```bash
# Serverless reclaim with EC consolidation
grpcurl -plaintext -d '{
  "payload_json": "{\"operations\":[\"ec_consolidate\",\"checkpoint\"]}"
}' localhost:50051 reddb.v1.RedDb/ServerlessReclaim
```

The `flush()` method also auto-consolidates, so any path that calls `flush()` before shutdown is safe.

---

## Deployment Mode Comparison

| Feature | Server | Embedded | Serverless |
|:--------|:-------|:---------|:-----------|
| Async consolidation | Background worker thread | Manual (`ec_consolidate`) or on `flush()` | Background worker during warm phase |
| Sync consolidation | Immediate on every mutation | Immediate on every mutation | Immediate on every mutation |
| Auto-consolidate on flush | Yes | Yes | Yes |
| HTTP/gRPC API | Full API | Not available | Full API |
| Reclaim hook | N/A | N/A | `ec_consolidate` operation |
| Persistence | Automatic with WAL | Explicit `flush()` | Explicit reclaim |

---

## Example: Click Counter

A high-throughput click counter that handles thousands of concurrent increments:

```bash
# Configure via red_config
curl localhost:8080/config -d '{
  "red.ec.urls.fields": "[\"clicks\"]",
  "red.ec.urls.clicks.reducer": "sum",
  "red.ec.urls.clicks.mode": "async",
  "red.ec.urls.clicks.interval_secs": "10"
}'

# Create a URL
curl localhost:8080/query -d '{
  "query": "INSERT INTO urls (slug, url, clicks) VALUES (\"home\", \"https://example.com\", 0)"
}'

# High-frequency click tracking (each returns immediately)
curl -X POST localhost:8080/ec/urls/clicks/add -d '{"id": 1, "value": 1}'
curl -X POST localhost:8080/ec/urls/clicks/add -d '{"id": 1, "value": 1}'
curl -X POST localhost:8080/ec/urls/clicks/add -d '{"id": 1, "value": 1}'

# After ~10 seconds, the worker consolidates automatically
curl localhost:8080/ec/urls/clicks/status?id=1
# → { "consolidated": 3.0, "pending_transactions": 0, ... }
```

---

## Example: Financial Ledger (Sync Mode)

For financial operations where read-after-write consistency is required:

```rust
let mut config = EcFieldConfig::new("accounts", "balance");
config.mode = EcMode::Sync; // consolidate immediately
config.reducer = EcReducer::Sum;
db.ec_register(config);

// Each operation consolidates before returning
db.ec_add("accounts", "balance", account_id, 1000.0)?; // deposit
db.ec_sub("accounts", "balance", account_id, 250.0)?;   // withdrawal

// Balance is immediately correct
let status = db.ec_status("accounts", "balance", account_id.raw());
assert_eq!(status.consolidated, 750.0);
```

---

## Internals

### Storage model

EC transactions are stored as regular RedDB entities in internal collections. This means they benefit from all existing infrastructure:

- **Persistence**: Transactions survive restarts (paged storage or binary file)
- **CDC**: Transaction creation emits CDC events
- **WAL replication**: Transactions replicate to primary's WAL buffer automatically
- **Backup**: Snapshots include `red_ec_tx_*` collections
- **Indexing**: Transaction collections participate in context index

### Cohort bucketing

Each transaction is assigned a `cohort_hour` string (e.g., `"2025-04-11T14"`) based on its creation timestamp. This enables efficient time-windowed queries during consolidation without scanning the entire transaction log.

### The SET operation

A `Set` operation acts as a checkpoint. During consolidation, the algorithm finds the last `Set` and discards all prior `Add`/`Sub` operations. This is useful for periodic rebalancing:

```bash
# After many adds and subs, reset to a known value
curl -X POST localhost:8080/ec/wallets/balance/set -d '{"id": 42, "value": 500}'
# All prior pending transactions before this SET are effectively ignored
```

### Idempotency

Consolidation is idempotent. Running it multiple times on the same set of transactions produces the same result because:

1. Transactions are marked `applied = true` after consolidation
2. Subsequent consolidation runs skip applied transactions
3. The consolidated value is deterministic from the transaction ordering
