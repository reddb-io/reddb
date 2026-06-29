# ADR 0064 - Placement Authority and Range Owner

Status: accepted
Date: 2026-06-29

Extends [ADR 0037](0037-shard-range-ownership-catalog.md),
[ADR 0052](0052-cluster-supervisor-control-plane-consensus.md),
[ADR 0055](0055-cluster-slot-map-and-cross-shard-operations.md), and
[ADR 0058](0058-cluster-bootstrap-authority.md).

RedDB cluster mode already separates the control plane from the data plane:
membership and ownership transitions are control-plane facts, while user writes
flow through per-range WAL, replication, and ownership fencing. As the cluster
grows to hundreds of nodes and thousands of collections, a single global
placement leader would become both a throughput bottleneck and an operational
blast radius. This ADR names the two authority roles that keep that boundary
clear while allowing the placement/control surface to scale out.

## Decision

**Placement Authority is the control-plane authority for one collection group.**
A Placement Authority owns a slice of the shard ownership catalog scoped to a
collection group/keyspace. It decides and publishes:

- the ranges that belong to collections in that group;
- the current range owner for each range;
- hot replicas and archive replicas for each range;
- ownership epochs and catalog versions;
- placement transitions such as split, move, rebalance, and promote.

It does not accept or apply user-data writes. It governs where ranges live and
who may own them; it does not govern the bytes inside those ranges.

**Range Owner is the data-plane writer for one range at one epoch.** A Range
Owner accepts writes only for `(collection, range_id, ownership_epoch)` that the
current catalog still assigns to it. It owns the range's write path:

- append user writes to the data WAL;
- apply writes to local storage;
- stream changes to range replicas;
- maintain the range commit watermark;
- serve snapshots and catch-up streams;
- reject stale writes when the expected epoch no longer matches.

**The authority-sharding unit is collection group/keyspace.** Small collections
may share a Placement Authority. A large collection's operational domain may
live in its own collection group. Range-level ownership remains finer-grained
than the Placement Authority: one collection group can contain many ranges, and
each range has exactly one Range Owner for a given ownership epoch.

**A range has exactly one Placement Authority and one Range Owner per epoch.**
Placement Authorities may be distributed across the cluster, but their
ownership-catalog slices must not overlap. A Range Owner is authoritative only
while the catalog slice's current epoch says it is. This prevents the two
split-brain classes that matter most:

- two Placement Authorities publishing conflicting owners for the same range;
- two Range Owners accepting writes for the same range and epoch.

**Triple replication distinguishes hot promotion from archive recovery.**
Production clustered ranges use a replication factor that includes:

- the current Range Owner;
- at least one hot mirror that can be promoted when it covers the range commit
  watermark and wins the ownership transition;
- an archive replica that may be compressed and optimized for restore.

The archive replica is not a direct write owner. It may become a recovery source
only after restore, checksum validation, and watermark validation. If it does
not cover the latest committed watermark, the forced recovery workflow must
surface the resulting RPO/skipped-data evidence.

**Serving graph and routing caches consume Placement Authority output.** Routers,
drivers, and data members may cache topology/serving-graph projections from the
Placement Authority, but cached topology is not a write authority. Correctness
still depends on Range Owner epoch fencing.

## Considered options

- **One global placement leader.** Rejected because it centralizes all
  ownership transitions for all collections/ranges. At 1000+ collections and
  100+ nodes it becomes a bottleneck and increases the impact of an overloaded
  or unavailable control-plane leader.
- **Placement Authority per collection.** Rejected as the default because many
  small collections do not need independent placement authority; it would create
  unnecessary operational fragmentation and metadata churn.
- **Placement Authority per range.** Rejected as the default because it makes
  the control plane too fine-grained to operate and reason about, and complicates
  group-level failover/rebalance policy.
- **Collection group/keyspace Placement Authority (chosen).** Balances scale and
  operator comprehension: placement authority can be distributed, while related
  collections keep one coherent operational scope.
- **Range Owner chooses its own successor.** Rejected because persistence nodes
  should not be their own placement authority. Promotion must be an ownership
  transition under the Placement Authority/control-plane contract.
- **Archive replica can promote directly.** Rejected because compression and
  restore optimization are not equivalent to hot write readiness. Archive
  recovery must prove restored bytes, checksums, and commit watermark coverage
  before ownership moves.

## Consequences

- Cluster terminology should use **Placement Authority** for control-plane
  ownership-catalog authority and **Range Owner** for data-plane write authority.
- Future serving-graph APIs need to expose collection group scope, catalog
  version, range owner, hot replicas, archive replicas, and ownership epoch.
- Failover workflows first ask the Placement Authority for a new ownership
  transition, then promote only a candidate whose range data covers the required
  commit watermark.
- Move/split/rebalance workflows are scoped to a collection group, but the
  actual handoff remains per range and per epoch.
- Control-plane consensus remains control-plane only. User-data writes still do
  not enter the Placement Authority log; they stay on the Range Owner data path.
- Tests for this model should prove that stale topology cannot produce a
  split-brain write and that archive recovery cannot skip validation before
  promotion.
