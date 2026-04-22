# Distributed Roadmap

RedDB today is single-node with WAL-based replication + quorum
semantics. Distributed query, automatic failover, and sharding are
on the roadmap; this page is the honest state-of-the-art and what
we're building toward.

## What exists today

* **WAL replication** — primary writes to WAL; replicas pull and
  apply. Async by default, sync via quorum policy. See
  `src/replication/primary.rs`, `replica.rs`, `quorum.rs`.
* **Quorum policies** — `Async`, `Sync { min_replicas }`,
  `Regions { required }`. Wired through the commit path.
* **Serialisable batches** — the `ColumnBatch` format introduced in
  B1 is explicitly laid out so a future network layer can ship
  batches between nodes without re-serialising.

## What's missing

| Capability | Status |
|------------|--------|
| Sharding (partition routing across nodes) | Not started |
| Distributed query (plan → shards → merge) | Not started |
| Automatic failover (leader election) | Not started |
| Cross-region replication | Async log-shipping works; needs tooling |
| Raft consensus for catalog | Not started |

## Target architecture

```
              ┌─────────────────────────────┐
              │      Coordinator node       │
              │  planner + shard router     │
              └──────────────┬──────────────┘
                             │
      ┌──────────────────────┼──────────────────────┐
      ▼                      ▼                      ▼
┌───────────┐          ┌───────────┐          ┌───────────┐
│  Shard A  │          │  Shard B  │          │  Shard C  │
│ data ⇢ local │       │ data ⇢ local │       │ data ⇢ local │
└───────────┘          └───────────┘          └───────────┘
```

Query flow:

1. Client sends SQL to any node (acts as coordinator).
2. Planner decides whether the query needs fan-out.
3. Sub-plans ship to owning shards, execute locally.
4. Coordinator merges partial results.

## Pre-requisites the TS/CH parity sprint lays down

* **`ColumnBatch` as the wire format** — B1 already designed to be
  zero-copy-ish: columns are contiguous buffers, schema is an
  `Arc<Schema>` a network layer can serialise with a preamble.
* **Projections** (B5) — pre-aggregated state lives adjacent to a
  shard's data; the coordinator merges `AggregateResult`s, not raw
  rows.
* **Partition pruning** (B7) — tells the coordinator which shards
  can be skipped entirely for a given predicate.
* **Parallel aggregate with partial-state merge** (B6) — the
  per-thread merge logic translates directly to per-shard merge.

## Phasing

| Phase | Content | Estimate |
|-------|---------|----------|
| D1 | Shard registry + consistent-hash router per table | 1 sprint |
| D2 | Distributed scan: ship sub-plan, collect batches, local merge | 2 sprints |
| D3 | Distributed aggregate: ship partial state, merge at coordinator | 1 sprint |
| D4 | Raft catalog (schema changes globally consistent) | 2 sprints |
| D5 | Auto-failover: leader lease + replica promotion | 1 sprint |

Total: ~1 trimester of focused work **after** the TS/CH parity
cycle closes. No commits to a ship date — this page is honest about
the gap so callers can plan.

## Non-goals

* Multi-leader writes — stays single-leader per shard. We avoid the
  conflict-resolution tarpit.
* Global secondary indexes — indexes stay shard-local; cross-shard
  uniqueness is enforced with a compensating write.
* ACID across shards — each transaction remains shard-local.
  Cross-shard atomicity arrives via saga patterns first; 2PC only
  if data supports it.
