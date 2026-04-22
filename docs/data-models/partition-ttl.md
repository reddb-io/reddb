# TTL at the Partition Level

RedDB supports three complementary expiry surfaces. Picking the
right one determines how fast data disappears and how much it costs
to reclaim:

| Surface                     | Granularity | Cost per expired row | When to use |
|-----------------------------|-------------|---------------------|-------------|
| **Entity TTL** (`WITH TTL` on INSERT) | Row-by-row | O(n) scan on sweep | Mixed-lifetime rows inside one collection (session tokens alongside durable users) |
| **Retention policy** (`add_retention_policy`) | Collection-wide chunk | O(1) chunk metadata drop | You want "keep last N days" managed by a daemon |
| **Partition TTL** (this page) | Chunk, declared at DDL time | O(1) chunk metadata drop | You want "keep last N days" declared alongside the table |

Partition TTL is the fastest and the most declarative. A single
metadata sweep reclaims every row of every expired chunk — no
per-row scan, no WAL growth from DELETE tombstones.

---

## Declaration

```sql
CREATE HYPERTABLE access_log (
  ts BIGINT, service TEXT, message TEXT
)
CHUNK_INTERVAL '1 day'
WITH (ttl = '90 days');
```

The `WITH (ttl = '...')` clause attaches a **default TTL** to every
chunk the hypertable allocates. A chunk is safely droppable once:

```
now_ns  ≥  chunk.max_ts_ns  +  chunk.effective_ttl_ns
```

`max_ts_ns` is the latest row the chunk has ever accepted — not the
chunk's declared boundary — so a chunk that only ever received data
near its start isn't kept artificially alive by an empty tail.

Duration grammar accepts the same forms as `CHUNK_INTERVAL`:
`30s`, `5m`, `6h`, `7d`, etc.

---

## Programmatic API

When you're driving RedDB in-process (e.g. from the `LogPipeline`
helper) the same knobs sit on `HypertableRegistry`:

```rust
use reddb::storage::timeseries::{HypertableRegistry, HypertableSpec, LogPipeline};

// At declaration time
let spec = HypertableSpec::new("access_log", "ts", 86_400_000_000_000)
    .with_ttl("90d").unwrap();
let registry = HypertableRegistry::new();
registry.register(spec);

// Or after the fact
registry.set_default_ttl_ns("access_log", Some(30 * 86_400_000_000_000));

// Run one sweep cycle
let now_ns = unix_now_ns();
let dropped = registry.sweep_expired("access_log", now_ns);
println!("released {} chunks", dropped.len());

// Or via the log pipeline wrapper
let pipe = LogPipeline::new("access_log", "ts", "1d").unwrap();
pipe.set_partition_ttl("90d");
let dropped = pipe.sweep_expired_chunks(now_ns);
```

---

## Per-chunk overrides — mixed TTL in one hypertable

Regulatory / incident-replay data sometimes needs to outlive the
default retention. Instead of splitting into two hypertables, you
raise the TTL on individual chunks:

```rust
// A chunk from the January incident — keep it for 7 years.
registry.set_chunk_ttl_ns(&id, Some(7 * 365 * 86_400_000_000_000));

// A backfill chunk we'll redo tomorrow — expire fast.
registry.set_chunk_ttl_ns(&other_id, Some(3_600_000_000_000));

// Clear the override and fall back to the hypertable default.
registry.set_chunk_ttl_ns(&normal_id, None);
```

The `sweep_expired` loop respects overrides: `effective_ttl =
chunk.ttl_override_ns.or(spec.default_ttl_ns)`.

---

## Interaction with other surfaces

* **Retention daemon** (`RetentionRegistry`) and partition TTL are
  additive. If both fire, the chunk disappears — the first pass
  wins, the second is a no-op. Declare partition TTL for DDL-level
  clarity; let the retention daemon policy layer on top for
  operational tuning.
* **Entity TTL** (per-row `WITH TTL`) keeps working inside live
  chunks. The row-level sweep cleans rows with an expired
  `expires_at_ms`; the partition sweep cleans *whole* chunks. If a
  chunk still has lively rows, the partition sweep leaves it
  alone.
* **Continuous aggregates** are unaffected — the materialised
  buckets are their own collection with its own retention. Drop raw
  chunks aggressively; keep rolled-up summaries forever.

---

## Preview sweep ("what will drop?")

```rust
// Chunks that will expire within the next 24h
let about_to_go = registry.chunks_expiring_within(
    "access_log",
    now_ns,
    24 * 3_600_000_000_000,
);
for chunk in about_to_go {
    println!("{:?} expires at {}", chunk.id, chunk.expiry_ns(spec.default_ttl_ns).unwrap());
}
```

Useful in dashboards ("storage that rolls off in the next week")
and in alerts ("retention misconfigured — about to lose
regulatory-window data").

---

## Cost model

* **Declaration cost**: zero. The TTL is one `Option<u64>` on the
  spec.
* **Write cost**: zero. Ingest never consults the TTL.
* **Sweep cost**: one metadata lock + one linear scan over the
  chunk BTree (`O(chunks)`). No row scans. No WAL writes beyond the
  chunk-removal entry.
* **Storage cost**: zero. The chunks themselves don't grow because
  of a TTL declaration.

---

## Comparison — Timescale / ClickHouse / Postgres

| System       | Partition-level TTL? | Cost of sweep |
|--------------|----------------------|---------------|
| TimescaleDB  | `add_retention_policy` + `drop_chunks`. Similar pattern, explicit call | O(chunks) |
| ClickHouse   | `TTL date + INTERVAL 90 DAY DELETE` on table DDL | O(parts) background merge |
| Postgres (declarative partitioning) | Manual — drop partitions with cron | O(partitions) |
| **RedDB**    | `WITH (ttl = '90 days')` on hypertable DDL + optional per-chunk override | **O(chunks)** no row scans |

The ClickHouse model is closest to ours; Timescale requires a
separate retention call; vanilla Postgres leaves the operator to
script it. RedDB lands in the declarative camp with the extra knob
of per-chunk overrides for mixed-policy hypertables.

---

## FAQ

**Q: My TTL says `90d` but a chunk older than 90 days is still
there.**
Expiry is `max_ts_ns + ttl`, not `chunk_start + ttl`. If a chunk's
newest row landed 10 days ago, the chunk lives for another 80.
This matches the Timescale semantic and stops a laggy late-arriving
row from losing its data prematurely.

**Q: Can I TTL a single partition of a classic `PARTITION BY RANGE`
table (not a hypertable)?**
The metadata path is the same — set `ttl_override_ns` on the
specific `ChunkMeta`. The DDL sugar (`ALTER TABLE ... SET TTL`) on
non-hypertable partitions is on the roadmap; use the programmatic
API for now.

**Q: Does partition TTL fire automatically?**
The sweep function runs when a daemon (the retention daemon, or
your own maintenance loop) calls it. Production installations run
the retention daemon every 60s by default — configured via
`red.config.retention.interval_ms`. A cold install with no daemon
doesn't auto-sweep.

**Q: Can I combine partition TTL with `APPEND ONLY`?**
Yes, and they pair well. Append-only + partition TTL is the
"write once, forget automatically" shape most log / audit /
telemetry workloads want. No UPDATE path means the codec and
layout decisions stay simple; TTL reclaims storage without any
human ceremony.
