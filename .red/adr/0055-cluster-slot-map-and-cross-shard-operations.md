# ADR 0055 — Cluster slot map and bounded cross-shard operations

Status: proposed
Date: 2026-06-15

Extends [ADR 0037](0037-shard-range-ownership-catalog.md),
[ADR 0044](0044-primary-replica-runtime-boundary.md),
[ADR 0045](0045-cluster-range-file-layout.md), and
[ADR 0052](0052-cluster-supervisor-control-plane-consensus.md).

RedDB cluster mode partitions user collections horizontally. The control-plane
work already chose a versioned shard/range ownership catalog; this ADR fixes the
user-data sharding model on top of it: how a row/document/key reaches a shard,
how primary-replica HA composes with cluster partitioning, and what cross-shard
queries may do in the first productive cluster cut.

## Decision

**Hash-partitioned collections route through fixed hash slots.** For hash-mode
collections, routing is:

```text
collection/table -> shard key -> hash -> slot -> shard group -> owner node
```

The production slot count is 16,384. The slot count is a cluster-format constant,
not `node_count`; adding or removing nodes must not remap the whole keyspace.
Small slot counts may be used only in tests or throwaway development catalogs
before cluster metadata becomes stable.

**The user chooses the logical shard key; RedDB chooses physical ranges.** These
are separate responsibilities:

| Decision | Owner | Contract |
|----------|-------|----------|
| Which field/expression places a row/document/key | User DDL, or an explicit RedDB collection-kind default | Stable logical contract: `SHARD BY tenant_id`, primary key, document id, etc. |
| Whether that key routes by hash slots or ordered key ranges | RedDB default plus optional advanced DDL | Hash is the default for distribution; ordered is opt-in for locality/scans. |
| Which slot/range belongs to which shard group | RedDB Cluster Supervisor and shard ownership catalog | Internal control-plane state; may change through split/move/rebalance. |
| Which node is current writer for a range | RedDB ownership transition | Internal and versioned by owner, epoch, replicas, and catalog version. |

The operator should not hand-place ordinary data into "range 0 to 1000" or
`hash(key) % node_count`. For a normal hash-partitioned table, the product
contract is:

```sql
CREATE TABLE orders (
  tenant_id TEXT,
  order_id TEXT,
  status TEXT
) SHARD BY tenant_id;
```

At runtime RedDB maps that logical key internally:

```text
tenant_id='acme' -> hash -> slot 9381 -> range [8192, 12288) -> shard group B -> owner node
```

RedDB may later split `[8192, 12288)` or move it to another owner without
changing the user's `SHARD BY tenant_id` contract.

**The shard ownership catalog remains the source of truth.** A slot is the
logical hash bucket. A range is still the cataloged ownership unit. In hash mode,
a `RangeOwnership` entry represents a contiguous span of hash slots, and the
catalog/cluster map records which shard group owns each span. RedDB may split or
move ranges to rebalance slots, but ownership is never recalculated from live
membership alone.

**A shard group is a small primary-replica group.** Each shard group owns one or
more slot spans and has exactly one write primary plus a configured set of
replicas. Production HA groups use `N` replicas according to the replication
factor; zero-replica groups are limited to development or explicitly non-HA
postures. The group uses the same commit-watermark, replica-read, and promotion
rules as primary-replica deployments. Cluster mode is therefore many
independently-owned primary-replica groups under one control plane:
primary-replica solves HA/read scaling inside a group; cluster mode solves
horizontal partitioning across groups.

**Cluster collections must have a shard key.** DDL must either declare `SHARD BY`
or rely on a collection-kind default that is explicit in the product contract.
Defaults are allowed only when they are stable, high-cardinality, and aligned
with normal access patterns:

- KV: key.
- Documents: document id, or tenant id when tenancy is declared.
- Tables: primary key, or explicit `SHARD BY`.
- Queues: queue name or tenant id.
- Timeseries/logs: tenant id plus a bucket; never timestamp alone.
- Graph: tenant id or graph id until a graph-specific placement model exists.

Examples:

```sql
CREATE TABLE orders (
  tenant_id TEXT,
  order_id TEXT,
  status TEXT
) SHARD BY tenant_id;

CREATE COLLECTION events SHARD BY tenant_id;
```

**Single-shard reads and writes are the happy path.** When a query contains the
shard key, the planner resolves the slot, routes to one shard group, and executes
there. This is the only write path supported by default in the first productive
cluster cut.

**Cross-shard reads are explicit and bounded.** Queries without a usable shard
key may scatter to multiple shard groups only when the request surface makes
fanout explicit or the planner can prove the fanout is within configured budgets.
The coordinator must enforce:

- timeout per shard leg;
- maximum shard-group count;
- maximum rows or bytes per shard leg;
- coordinator memory budget;
- merge/sort/limit budget;
- partial response only when the query explicitly allows partial results;
- tracing that names participating shard groups, timeouts, retries, and partial
  result status.

Best-effort cross-shard reads do not claim global snapshot consistency.
Consistent cross-shard reads require a safe global watermark that covers every
participating range, matching the existing `GlobalReadWatermark` shape.

**Cross-shard write transactions are rejected in v1.** A transaction whose write
set spans shard groups owned by different writers fails with an explicit
unsupported/cross-shard error. RedDB must not do partial commits and must not
introduce two-phase commit in the first sharded cluster milestone. A future
special API may support saga-style workflows or a narrower distributed
transaction model, but that is a separate decision.

**Cross-shard joins and global secondary indexes are out of scope initially.**
Shard-local indexes are supported. Global secondary indexes, cross-shard joins,
and global uniqueness constraints require separate design because they turn many
ordinary writes into distributed writes.

**Global aggregations use scatter-gather with partial state.** Aggregations such
as global top-N, counts, maxima, leaderboards, and recent-events views may execute
by asking each shard group for bounded partial results and merging them at the
coordinator. The coordinator should merge partial aggregate state, not raw
unbounded rows, whenever the query shape allows it.

Example classifications:

```sql
-- orders is SHARD BY tenant_id: one slot, one shard group.
SELECT * FROM orders WHERE tenant_id = 't1' AND order_id = 'o1';

-- No shard key predicate: bounded cross-shard read if the request allows fanout.
SELECT * FROM orders WHERE status = 'pending';

-- Global top-N: cross-shard read using per-shard partial top-N plus coordinator merge.
SELECT * FROM events ORDER BY created_at DESC LIMIT 100;
```

A transfer between users or accounts on different shard groups is a cross-shard
write transaction and is rejected by default in v1.

**Caching is layered and never an ownership authority.**

- Routing cache: clients and nodes cache `(collection, slot) -> owner, replicas,
  ownership epoch, cluster-map epoch/catalog version`. A stale route receives a
  redirect/MOVED-like response carrying the slot, current owner address,
  ownership epoch, and catalog version, for example
  `MOVED slot=1234 owner=node-b:55055 epoch=42`. The cache updates and retries,
  but correctness still depends on the owner's fencing gate.
- Cross-shard result cache: expensive global reads may cache
  `query_hash + catalog_version` with a short TTL such as 5s, 30s, or 5m depending
  on freshness requirements. This cache is an optimization; a consistent read
  must include the safe watermark in the cache key or bypass the entry.
- Per-shard partial cache: shard groups may cache local top-N, counts, max
  timestamp, and similar partials so coordinators merge small bounded results.

**Hot-key splitting is explicit and deferred.** Plain `SHARD BY tenant_id` can
create a hot shard when one tenant dominates traffic. The first cut relies on
range split/move and operator visibility. A later DDL extension may opt a
collection into compound hot-key striping, for example:

```sql
SHARD BY tenant_id SPLIT HOT KEY BY entity_id INTO 16 BUCKETS
```

That mode intentionally increases cross-shard work inside a tenant, so it must
be explicit rather than a default.

## Considered options

- **`hash(shard_key) % node_count`.** Rejected because adding or removing a node
  reshuffles most keys and makes rebalancing operationally expensive.
- **Fixed hash slots with cataloged ownership spans (chosen).** Keeps routing
  simple, gives the control plane a stable keyspace to move gradually, and still
  compresses metadata as ranges instead of storing one row per slot.
- **Pure hash-token ranges with no slot language.** Rejected as the product
  contract because fixed slots are easier to explain, cache, redirect, and
  operate. Internally, ranges may still be represented as slot spans.
- **Directory-only sharding.** Rejected for the default path because every lookup
  would depend on an extra directory read. The ownership catalog remains global
  control-plane state, but point routing is formulaic: shard key -> hash -> slot.
- **Two-phase commit for cross-shard writes.** Rejected for v1 because it adds
  blocking coordinator failure modes before the single-shard and bounded-read
  paths are mature.

## Consequences

- The query analyzer needs to identify shard-key predicates and mark a plan as
  single-shard, bounded cross-shard read, or unsupported cross-shard write.
- DDL needs a stable `SHARD BY` surface and collection-kind defaults.
- Topology snapshots and routing hints need slot/span metadata in addition to
  owner, replicas, ownership epoch, and catalog version.
- The transport needs a redirect/MOVED-like payload for stale slot ownership.
- Scatter-gather execution needs budgets, cancellation, partial-result semantics,
  and trace fields before it is safe to expose broadly.
- Result caching must include catalog generation and, for consistent reads, the
  global read watermark; otherwise cached global results may be used only under
  documented TTL staleness.
- Placement and rebalancing should plan moves over slot spans/ranges, preserving
  the range file layout from ADR 0045.
