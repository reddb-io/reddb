# Distributed Roadmap

RedDB today has single-node storage plus WAL-based replication and
quorum semantics. Distributed query, automatic failover, and
multi-writer sharding are on the roadmap; this page is the honest
state-of-the-art and what we're building toward. The target sharding
contract is documented in [Cluster Sharding Contract](cluster-sharding.md).

## What exists today

* **WAL replication** — primary writes to WAL; replicas pull and
  apply. Async by default, sync via quorum policy. See
  `src/replication/primary.rs`, `replica.rs`, `quorum.rs`.
* **Quorum policies** — `Async`, `Sync { min_replicas }`,
  `Regions { required }`. Wired through the commit path.
* **Cluster control-plane shape** — ADR 0037 chooses a versioned
  shard/range ownership catalog, ADR 0045 chooses range-oriented
  cluster storage, and ADR 0052 chooses a Raft-equivalent
  control-plane log for membership and ownership transitions only.
* **Range-authority substrate** — logical replication records can
  carry range identity and ownership epoch, and the control-plane
  seam accepts membership changes and ownership transitions without
  allowing user data into the control-plane log.
* **Serialisable batches** — the `ColumnBatch` format introduced in
  B1 is explicitly laid out so a future network layer can ship
  batches between nodes without re-serialising.

## What's missing

| Capability | Status |
|------------|--------|
| Durable shard/range ownership catalog | Designed; implementation incomplete |
| Sharding router across nodes | Designed; implementation incomplete |
| Split/move/merge/rebalance transitions | Designed; implementation incomplete |
| Distributed query (plan → shards → merge) | Not started |
| Automatic failover (leader election + committed control-plane log) | Election substrate exists; committed log incomplete |
| Cross-region replication | Async log-shipping works; needs tooling |
| Raft-equivalent consensus for catalog transitions | ADR accepted; implementation incomplete |

## Target architecture

```
              ┌─────────────────────────────┐
              │      Coordinator node       │
              │ planner + range router       │
              └──────────────┬──────────────┘
                             │
      ┌──────────────────────┼──────────────────────┐
      ▼                      ▼                      ▼
┌───────────┐          ┌───────────┐          ┌───────────┐
│ Range A   │          │ Range B   │          │ Range C   │
│ data local│          │ data local│          │ data local│
└───────────┘          └───────────┘          └───────────┘
```

Query flow:

1. Client sends SQL to any node (acts as coordinator).
2. Planner and router resolve predicates to owned shard/ranges.
3. Sub-plans ship to owning range members, execute locally.
4. Coordinator merges partial results.

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
| D1 | Durable ownership catalog + range router | 1 sprint |
| D2 | Ownership fencing + stale-owner retry path | 1 sprint |
| D3 | Distributed scan: ship sub-plan, collect batches, local merge | 2 sprints |
| D4 | Distributed aggregate: ship partial state, merge at coordinator | 1 sprint |
| D5 | Committed control-plane log for membership and ownership transitions | 2 sprints |
| D6 | Auto-failover: range-owner lease + replica promotion | 1 sprint |

Total: ~1 trimester of focused work **after** the TS/CH parity
cycle closes. No commits to a ship date — this page is honest about
the gap so callers can plan.

## Non-goals

* Multi-leader writes — stays single-leader per shard/range. We avoid the
  conflict-resolution tarpit.
* Global secondary indexes — indexes stay shard-local; cross-shard
  uniqueness is enforced with a compensating write.
* ACID across shards — each transaction remains shard-local.
  Cross-shard atomicity arrives via saga patterns first; 2PC only
  if data supports it.
* Formula-only placement — `hash(key) % N` is not the live cluster
  routing contract. Hashing can seed balanced ranges, but the ownership
  catalog is authoritative after bootstrap.
