# Distributed Roadmap

RedDB today ships production deployment modes for embedded/server,
serverless-style remote storage, and primary-replica WAL replication with quorum
semantics. The cluster sharding control-plane model has landed, but the full
multi-writer runtime is not a production claim yet. This page is the honest
state-of-the-art and what we're building toward. The target sharding contract
is documented in
[Cluster Sharding Contract](cluster-sharding.md).

## What exists today

* **WAL replication** — primary writes to WAL; replicas pull and
  apply. Async by default, sync via quorum policy. See
  `src/replication/primary.rs`, `replica.rs`, `quorum.rs`.
* **Quorum policies** — `Async`, `Sync { min_replicas }`,
  `Regions { required }`. Wired through the commit path.
* **Cluster sharding control plane** — a range ownership catalog, hash/ordered
  collection sharding modes, owner epochs, stale-version rejection, any-node
  routing decisions, and cross-range guardrails. See
  [Cluster Sharding](./cluster-sharding.md).
* **Fixed hash-slot primitive** — `slot.rs` ships the production 16,384-slot
  map and the stable `shard_key -> hash -> slot -> range-key` function, so
  hash-mode ranges are slot spans rather than `hash(key) % node_count`. The
  primitive is consumed by the ownership/topology models but not yet wired into
  a serving path, and there is no public `SHARD BY` DDL yet.
* **Serialisable batches** — the `ColumnBatch` format introduced in
  B1 is explicitly laid out so a future network layer can ship
  batches between nodes without re-serialising.
* **Cluster control-plane model** — ADR 0037 defines versioned
  shard/range ownership, ADR 0045 defines range-oriented cluster file
  layout, and ADR 0052 (accepted) fixes the control-plane consensus
  boundaries: a Raft-equivalent log governs membership, leader election,
  and ownership-catalog transitions *only*, while user-data writes stay
  out of that log. The decision is accepted; the durable replicated
  control-plane log itself is a named follow-up slice, not yet built.
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
| Sharding control-plane model | Landed; runtime integration in progress |
| Production multi-writer cluster serving | In progress |
| Distributed query (plan → shards → merge) | Not started |
| Automatic failover runtime wiring | In progress |
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

| Phase | Content | Status |
|-------|---------|--------|
| D1 | Range ownership catalog, hash/ordered collection modes, owner epochs, routing decisions | Landed as a pure control-plane model |
| D2 | Wire cluster routing/fencing into the serving request paths | In progress |
| D3 | Distributed scan: ship sub-plan, collect batches, local merge | Roadmap |
| D4 | Distributed aggregate: ship partial state, merge at coordinator | Roadmap |
| D5 | Raft catalog for globally consistent schema/control-plane changes | Boundary accepted (ADR 0052); replicated log slice roadmap |
| D6 | Auto-failover runtime wiring: leader lease + replica promotion | In progress |

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
