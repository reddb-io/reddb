# Cluster Sharding Contract

RedDB cluster sharding is a cataloged range-ownership design. It is not a
runtime formula such as `hash(key) % node_count` once the cluster is live.

This page documents the target contract and the current implementation boundary.
It connects the user-facing partitioning model with the cluster control-plane
ADRs so the words "range", "hash", and "shard" do not drift.

## Current Status

The multi-writer cluster data plane is not complete yet. The repository already
contains the design and some substrate:

- [ADR 0037](../../.red/adr/0037-shard-range-ownership-catalog.md): the
  ownership catalog is the source of truth for shard/range placement.
- [ADR 0045](../../.red/adr/0045-cluster-range-file-layout.md): cluster storage
  is physically organized around shard/ranges.
- [ADR 0052](../../.red/adr/0052-cluster-supervisor-control-plane-consensus.md):
  the Cluster Supervisor commits membership and ownership changes through a
  Raft-equivalent control-plane log.
- `replication::control_plane`: the code seam for membership changes and
  ownership transitions. It deliberately has no user-data entry.
- `replication::cdc` / `replication::logical`: range authority metadata and
  range-indexed stream primitives used by replication and future range movement.

What is still missing is the end-to-end cluster router, ownership catalog
storage, automatic split/move/rebalance, distributed execution, and the full
multi-writer runtime.

## Two Different Layers

RedDB has two related but different concepts:

| Layer | User sees | Purpose |
| --- | --- | --- |
| Logical partitioning | `PARTITION BY RANGE`, `LIST`, or `HASH` | Split a table/collection into child partitions for retention, pruning, and application-visible layout. |
| Cluster shard/range ownership | Shard/range catalog entries | Decide which cluster member owns or replicates each bounded unit of data. |

Logical partitioning is documented in
[Partitioning](../query/partitioning.md). It is a schema/query concept.

Cluster shard/range ownership is a physical placement and failover concept. The
Cluster Supervisor owns the live placement state, not ordinary table DDL.

## Range vs Hash

RedDB uses ranges as the cluster ownership unit. That does not mean every user
workload must be ordered by time or numeric ID.

There are two separate questions:

1. **How do we derive shard/range boundaries?**
   - Ordered mode creates ranges over an ordered shard key, such as time,
     account ID, or another key where locality matters.
   - Hash mode hashes the shard key first, then creates ranges over the hash
     token space. This gives even distribution for high-cardinality keys.

2. **Who owns each range right now?**
   - The ownership catalog records the owner, replicas, and ownership epoch for
     each range.
   - The catalog is authoritative after bootstrap. Hashing can propose an
     initial placement, but live routing, failover, split, merge, and rebalance
     read catalog state.

So the short answer is: **cluster ownership is range-based; hash is a way to
build balanced ranges.** A hash-sharded collection still becomes a set of
cataloged ranges.

RedDB should not route live cluster writes with plain `hash(key) % N`. That
scheme is useful as a teaching model and can work for static clusters, but adding
or removing nodes remaps too much data. RedDB's target is cataloged ownership
with split/move transitions, optionally helped by consistent-hash-style token
ranges during initial placement and resharding.

## Who Chooses The Shard Key?

The product contract is:

- If the workload has an obvious access pattern, the user should declare the
  shard key and mode. Examples: hash by `user_id` for user-scoped lookups, or
  ordered ranges by `ts` for time-series retention and range scans.
- If the user does not declare a cluster shard key, RedDB may choose a safe
  default for the collection, starting with one range and splitting later when
  statistics show size or load pressure.
- Operators and the Cluster Supervisor choose live range placement: owner,
  replicas, split points, moves, promotions, and epochs.

The user should not need to say "put this range on node 2" for the normal path.
That is an operational transition, not application schema. Authorized
administrative recovery commands may request move, split, merge, promote, or
forced transitions, but those still go through fenced ownership-transition
machinery.

## Routing And Safety Properties

The cluster router must resolve a request to a shard/range before it writes.
Correctness does not depend only on client-side routing.

Required properties:

- **Single writer per range.** Multiple members may be writers for different
  ranges, but one range has one writer at a time.
- **Epoch fencing.** Ownership transitions bump the ownership epoch. A stale
  former owner must reject writes locally even if a client routes to it.
- **Catalog authority.** The ownership catalog, not node membership alone, is
  the source of truth for range owner and replica placement.
- **Control/data-plane split.** The control-plane log commits membership and
  ownership transitions only. User writes stay in the data plane: WAL, logical
  stream, range replicas, and commit policy.
- **Stale routing correction.** A node receiving a request for a stale owner or
  epoch returns enough ownership metadata for the router or client to refresh and
  retry.

## Cross-Range Operations

Single-range operations are the fast path. Queries and transactions that touch
many ranges have different rules.

| Operation | First cluster cut |
| --- | --- |
| Point read/write on shard key | Route to one range owner. |
| Range scan spanning several ranges | Fan out to owners and merge results. |
| Global aggregate/top-N | Fan out, merge partials, and cache or precompute if it is hot. |
| Cross-range write transaction | Reject initially rather than risk partial commits. |
| Cross-range consistent read | Requires a global causal watermark or equivalent snapshot contract. |

The first cut should make cross-range reads possible but explicit. It should not
pretend every distributed query has a global transactional snapshot.

Expensive cross-range reads should use ordinary database techniques:

- prune ranges using the shard key and query predicate;
- cache hot global results with a clear freshness window;
- precompute leaderboards, rollups, and materialized projections;
- denormalize only when the read path justifies the write complexity.

Cross-range writes should use a saga/compensating workflow before RedDB grows a
two-phase-commit path. 2PC is not part of the first sharding milestone.

## Relationship To Deployment Modes

Cluster sharding is different from the other deployment modes:

- **Standalone / embedded file:** one process owns the whole local database.
- **Serverless:** one writer process uses local cache plus a remote backend; it
  is not a multi-writer sharded cluster.
- **Primary-replica:** one primary owns all writes; replicas follow the WAL.
- **Cluster:** many data members may own different shard/ranges at the same
  time, with single-writer authority per range.

Primary-replica and cluster share replication primitives, but primary-replica is
not sharding.

## Implementation Checklist

Before RedDB should call cluster sharding production-ready, these pieces must be
in place:

- durable ownership catalog with owner, replicas, bounds, and epoch;
- router that maps collection/key/predicate to catalog ranges;
- local write gate that enforces ownership and epoch fencing;
- range-indexed WAL/logical catch-up for movement and repair;
- Supervisor-owned split, move, merge, promote, and rebalance transitions;
- stale-ownership response and retry path in clients/routers;
- explicit cross-range read semantics and rejection of unsafe cross-range writes;
- tests for node add/remove, hotspot split, stale owner fencing, and recovery.
