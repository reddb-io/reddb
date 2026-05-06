# RedDB Performance Roadmap

Status: 2026-05-06 — canonical methodology is `make duel-official` per
issue #154 (`BenchConfigSchema`, `OFFICIAL_PROFILE=standard`,
`OFFICIAL_RUNS=10`, `ITEMS=50000`). The earlier mini-duel target
remains the dev variant; published numbers must come from
`duel-official`.

> **Where we win, where we lose.** Reproducible scenario-by-scenario
> positioning lives in [`wins.md`](wins.md) (the two productized wins:
> `typed_insert` ~16× over PG, `disk_usage` ~1.5× over Mongo) and
> [`when-not-reddb.md`](when-not-reddb.md) (the catalogued gaps with
> closure-issue links). This roadmap explains the engineering work
> behind both pages.

## Posture (ADR 0009)

PRD #152 ships against the **scenario-specific** posture chosen in
[ADR 0009](../adr/0009-performance-gate-scope.md): RedDB defends the
unified-engine wins (`typed_insert`, `disk_usage`, cross-model
queries) and narrows — not necessarily inverts — the gap scenarios
where storage-engine specialisation dominates (`concurrent`,
`bulk_update`, `aggregate_group`, `update_random`). Universal-20%
across the grid was rejected because the architectural commitments
required to defend it (sharded log, columnar push-down planner, MVCC
redesign) are each a multi-quarter PRD. The slices below are sized
to the chosen posture.

## Methodology

All benchmarks run via `make duel-official` in `rdb-benchmark`
(canonical) or `make mini-duel` (dev variant of the same schema).
One transport, apples to apples. Any result discussed in this
document or in `wins.md` / `when-not-reddb.md` must come from a run
made under this setup — no cherry-picking transports per scenario.
The methodology lock and the stale-binary preflight guards are
themselves tracked as roadmap slices (see #154 and #155 below).

## Server-side bottlenecks (PRD #152)

Listed by scope and return-on-investment. Each is a dedicated PR.
The original four-bottleneck list is preserved in place and
annotated with shipped status.

### 1. UnifiedRecord layout (scan hot paths) — ✅ SHIPPED (#156)

Outcome: schema-shared layout landed on `main`; +6 proptests, full
2886-test lib suite green. Closes the per-row `HashMap` allocation
that flamegraphs blamed for ~60% of CPU on `select_range`,
`select_filtered`, and `mixed_workload_indexed`. Re-bench against
the canonical lock will refresh the gap rows in
`when-not-reddb.md`.

**Original problem.** `UnifiedRecord` stored fields in
`HashMap<String, Value>`. Every scan row materialisation allocated
a fresh HashMap plus `N` owned String keys.

**Fix as shipped.** Schema-shared layout:

```rust
pub struct UnifiedRecord {
    schema: Arc<Vec<String>>,  // shared across all records of one result
    values: Vec<Value>,        // parallel to schema
    overflow: Option<HashMap<String, Value>>,  // only for ragged rows
}
```

`Arc::clone` per record instead of N heap allocations; values
access via binary search over the shared schema; overflow HashMap
only materialises for schemaless inserts.

### 2. WAL append lock-free path (concurrent writes) — ✅ SHIPPED (#157)

Outcome: `Mutex<WalWriter>` replaced by a lock-free SegQueue +
single-leader flush coordinator. 6 new coordinator tests plus the
full WAL suite green on `main`. The `concurrent` gap (~49× behind
Mongo at the lock window) is the headline target; re-bench tracked
in `when-not-reddb.md` Gap 1.

**Original problem.** Every commit took `Mutex<WalWriter>` across
`Begin + PageWrite×N + Commit`. Under 16-way concurrent workers,
inserts serialised on this mutex *and* on the state-condvar
notify_all thundering herd.

**Fix as shipped.** `crossbeam::queue::SegQueue<(u64 seq, Vec<u8>)>`
for pending encoded records; writers CAS a sequence from atomic
`next_seq` and push bytes; a single leader drains in LSN order,
fsyncs, publishes `durable_lsn` via atomic; waiters atomic-load
and park, leader `unpark_all` after publish. Commit-coordinator
state moved to `parking_lot` primitives.

### 3. Pager cache striped locks — ✅ SHIPPED (#158)

Outcome: cache sharded into N buckets each with its own RwLock,
`page_id % N` routing. Measured **5.6× speedup vs single-lock at
10 workers**. Internal refactor only, no API surface change.
SIEVE eviction reviewed across shards.

### 4. BTree batch upsert by leaf — ✅ SHIPPED (#159)

Outcome: `BTree::upsert_batch_sorted` helper + one caller change
in `persist_entities_to_pager`. Sorts keys within a single entity
batch, walks each leaf once, applies all updates for that leaf in
one page write. Proptest covers 1..200 entries. Targets the
~30× `bulk_update` gap in `when-not-reddb.md` Gap 2.

## In-flight slices (PRD #152)

These close the remaining `when-not-reddb.md` gaps that the four
shipped slices above did not eliminate.

### IncrementalIndexMaintainer — in flight (#160)

Closes `BASELINE` Finding #4 and the secondary half of
`when-not-reddb.md` Gap 4 (`select_filtered`). Maintains secondary
indices on every write so the planner can use them, instead of
falling back to a full-table scan. Restarted on a fresh `main`
base after the #156–#159 cluster landed.

### AggregateQueryPlanner — in flight (#161)

Closes the ~12× `aggregate_group` gap (`when-not-reddb.md` Gap 3).
Replaces the per-row HashMap rehydration with a columnar group-by
over the underlying page representation.

## Methodology and productization slices (PRD #152)

These are non-engine slices that PRD #152 also delivered. They are
listed here so the roadmap traces every PRD line item.

- **#154 — bench methodology lock-in.** ✅ SHIPPED in
  `rdb-benchmark`. Canonical = `make duel-official`. Every number
  in `wins.md` / `when-not-reddb.md` cites a session id from this
  configuration.
- **#155 — stale-binary preflight + autorebuild.** ✅ SHIPPED in
  `rdb-benchmark`. Prevents the entire class of regressions that
  came from running an outdated binary against an updated schema.
- **#163 — productize wins.** ✅ SHIPPED. `docs/perf/wins.md`
  lifts the two reproducible wins out of `BASELINE.md`;
  `docs/perf/when-not-reddb.md` is the honest counterpart with
  closure-issue links.
- **#153 — ADR 0009 posture.** ✅ SHIPPED. Scenario-specific
  posture recorded; see "Posture" section above.
- **#162 — close #124 not-a-regression.** ✅ SHIPPED inline.

## Topology discovery (PRD #164)

Server-advertised topology so clients learn the primary endpoint
and the replica fleet from whichever node they hit first, instead
of enumerating the cluster in their seed config. Throughput
motivation is replica-aware read routing. Security model is
recorded in [ADR 0008](../adr/0008-topology-advertisement-security.md).

### Slice DAG

```
#165 ADR 0008 (security model)        ── ✅ SHIPPED
        │
#166 wire payload spec                 ── ✅ SHIPPED
   (canonical Topology in
    crates/reddb-wire/src/topology.rs)
        │
        ├── #167 TopologyAdvertiser       ── in flight
        │       (server-side)
        │
        ├── #168 TopologyConsumer         ── in flight
        │       (client-side)
        │
        ├── #170 PerEndpointPool          ── ✅ SHIPPED
        │       (5.4× faster than
        │        legacy mutex pool)
        │
        ├── #171 HealthAwareRouter        ── in flight
        │
        └── #172 E2E integration test     ── pending
                (blocked on #167+#168+#171)
```

### State

- **#165 ADR 0008.** ✅ SHIPPED. Capability gate
  `cluster:topology:read`, default-on for authenticated principals,
  default-off for anonymous. Seed URI is a hint; advertised
  topology is authoritative on disagreement.
- **#166 wire payload spec.** ✅ SHIPPED. Canonical `Topology` type
  lives in `crates/reddb-wire/src/topology.rs`.
- **#170 PerEndpointPool.** ✅ SHIPPED. 5.4× faster than the legacy
  mutex pool under contended dispatch.
- **#167 TopologyAdvertiser** (server-side) — in flight.
- **#168 TopologyConsumer** (client-side) — in flight.
- **#171 HealthAwareRouter** — in flight.
- **#172 E2E integration test** — pending; blocked by
  #167 + #168 + #171.

## Deferred / out of scope

- **gRPC transport overhead** — `reddb_binary` adapter is slower
  because tonic does `spawn_blocking` per RPC (~150µs handoff).
  We no longer measure via gRPC; using wire consistently sidesteps
  the noise. Real fix = switch tonic to sync handlers, which is a
  tonic-wide change, not a RedDB one.

- **`select_complex` already at paridade** (1.15×). No action.

- **Universal-20% architectural rework** — sharded log structure,
  columnar push-down planner, MVCC redesign. Each is a
  multi-quarter PRD on its own; ADR 0009 records the explicit
  decision not to schedule them under PRD #152.

## Execution order

The original ROI ordering (#1 → #2 → #4 → #3) shipped end-to-end.
Remaining order is determined by gap rather than dependency:

1. **#160 IncrementalIndexMaintainer** — unblocks the planner half
   of `select_filtered` once #156's UnifiedRecord work is bench-
   confirmed.
2. **#161 AggregateQueryPlanner** — independent of #160. Pick up
   in parallel.
3. **PRD #164 client-side cluster** (#167 / #168 / #171 → #172) —
   independent of #160 / #161; closes the replica-aware routing
   throughput story.

## Non-goals

- Hitting PG parity on every scenario (we're a different database;
  ADR 0009 makes the posture explicit).
- Maintaining durability guarantees weaker than PG's
  `synchronous_commit=off` — async mode is the floor.
- Adding feature flags for each optimisation; land them
  unconditionally or don't land them.
