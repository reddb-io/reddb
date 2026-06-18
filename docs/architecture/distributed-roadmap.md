# Distributed Roadmap

RedDB today is single-node or primary-replica at runtime, with
WAL-based replication + quorum semantics. Distributed query,
automatic failover, and productive cluster sharding are on the
roadmap; this page is the honest state-of-the-art and what we're
building toward. The target sharding contract is documented in
[Cluster Sharding Contract](cluster-sharding.md).

## What exists today

* **WAL replication** — primary writes to WAL; replicas pull and
  apply. Async by default, sync via quorum policy. See
  `src/replication/primary.rs`, `replica.rs`, `quorum.rs`.
* **Quorum policies** — `Async`, `Sync { min_replicas }`,
  `Regions { required }`. Wired through the commit path.
* **Serialisable batches** — the `ColumnBatch` format introduced in
  B1 is explicitly laid out so a future network layer can ship
  batches between nodes without re-serialising.
* **Cluster control-plane model** — ADR 0037 defines versioned
  shard/range ownership, ADR 0045 defines range-oriented cluster file
  layout, and ADR 0052 defines control-plane consensus boundaries.
* **Range-authority substrate** — logical replication records can
  carry range identity and ownership epoch, and the control-plane
  seam accepts membership changes and ownership transitions without
  allowing user data into the control-plane log.
* **Pure cluster decision layers** — `crates/reddb-server/src/cluster/`
  contains pure ownership, topology, routing, placement, move-range,
  and cross-range planning models. These are not yet wired into the
  production query/write runtime.

## What's missing

| Capability | Status |
|------------|--------|
| Sharding model | Proposed in ADR 0055: shard key -> hash -> slot -> shard group -> owner |
| Partition routing across nodes | Pure catalog/routing model exists; runtime integration missing |
| Distributed query execution | Pure cross-range read/write planning exists; executor/coordinator missing |
| Automatic failover | Control-plane consensus boundary accepted in ADR 0052; runtime log/leader integration missing |
| Cross-region replication | Async log-shipping works; needs tooling |
| Control-plane catalog consensus | Designed; durable replicated control-plane log missing |

## Target architecture

```
              ┌──────────────────────────────┐
              │       Coordinator node       │
              │ planner + slot/range router  │
              └──────────────┬───────────────┘
                             │
                 cluster slot map / catalog
                             │
      ┌──────────────────────┼──────────────────────┐
      ▼                      ▼                      ▼
┌──────────────┐       ┌──────────────┐       ┌──────────────┐
│ Shard group A│       │ Shard group B│       │ Shard group C│
│ owner + reps │       │ owner + reps │       │ owner + reps │
└──────────────┘       └──────────────┘       └──────────────┘
```

User-facing sharding and internal placement are intentionally separate:

| Layer | Who chooses it | Example |
|-------|----------------|---------|
| Logical shard key | User DDL or explicit collection-kind default | `SHARD BY tenant_id`, primary key, document id |
| Shard key mode | RedDB default, with advanced opt-in modes later | Hash by default; ordered only when locality/scans justify it |
| Hash slot / range span | RedDB control plane | slot `9381` in range `[8192, 12288)` |
| Shard group / owner node | RedDB control plane | shard group B, current owner `node-b` |

For the normal hash path, a user writes `SHARD BY tenant_id`; RedDB maps each
`tenant_id` to a stable hash slot, groups contiguous slots into catalog ranges,
and moves or splits those ranges as the cluster rebalances. The user does not
pick physical ranges, and RedDB does not use `hash(key) % node_count`.

Query flow:

1. Client sends SQL to any node (acts as coordinator).
2. Planner extracts the shard key when possible.
3. Hash-mode collections route `shard key -> hash -> slot -> shard
   group -> owner`.
4. Single-shard reads/writes execute on one shard group.
5. Explicit bounded cross-shard reads scatter to participating shard
   groups, collect partial results, and merge/sort/limit at the
   coordinator.

Write flow:

1. Router resolves the write key to a catalog range and ownership epoch.
2. The owning member admits the write only if it still owns that epoch.
3. The write enters the data plane: WAL, logical stream, replicas, and
   commit policy.
4. Ownership changes are committed separately by the control plane; user data
   never enters the control-plane log.

## Pre-requisites the TS/CH parity sprint lays down

* **`ColumnBatch` as the wire format** — B1 already designed to be
  zero-copy-ish: columns are contiguous buffers, schema is an
  `Arc<Schema>` a network layer can serialise with a preamble.
* **Projections** (B5) — pre-aggregated state lives adjacent to a
  shard/range's data; the coordinator merges `AggregateResult`s, not raw
  rows.
* **Partition pruning** (B7) — tells the coordinator which shard/ranges
  can be skipped entirely for a given predicate.
* **Parallel aggregate with partial-state merge** (B6) — the
  per-thread merge logic translates directly to per-shard merge.

## Phasing

| Phase | Content | Estimate |
|-------|---------|----------|
| D1 | DDL shard key surface + slot map projected from the ownership catalog | 1 sprint |
| D2 | Single-shard router: point read/write to owner, stale-route redirect, routing cache | 1 sprint |
| D3 | Bounded distributed scan: fan-out sub-plans, collect batches, local merge | 2 sprints |
| D4 | Distributed aggregate: ship partial state, merge at coordinator, cache partials | 1 sprint |
| D5 | Control-plane catalog log: durable consensus for membership and ownership transitions | 2 sprints |
| D6 | Auto-failover: ownership lease, commit watermark check, replica promotion | 1 sprint |

Total: ~1 trimester of focused work **after** the TS/CH parity
cycle closes. No commits to a ship date — this page is honest about
the gap so callers can plan.

## Non-goals

* Multi-leader writes — stays single-leader per shard/range. We avoid the
  conflict-resolution tarpit.
* Global secondary indexes — indexes stay shard-local in the first
  cluster cut.
* Cross-shard joins — not exposed until the planner and memory budgets
  can enforce a safe distributed join contract.
* ACID across shard groups — each write transaction remains
  single-writer. Cross-shard atomicity arrives via explicit APIs or
  saga patterns first; 2PC only if data supports it.
* Timestamp-only sharding for logs/timeseries — use tenant/id plus a
  bucket; timestamp-only creates hot newest ranges.
* Formula-only placement — `hash(key) % N` is not the live cluster
  routing contract. Hashing can seed balanced ranges, but the ownership
  catalog is authoritative after bootstrap.
