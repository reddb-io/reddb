# Primary-replica Runtime Boundary

Status: proposed

RedDB primary-replica deployments use a single write primary, deterministic
replica replay, replica-aware read routing, causal freshness via bookmarks/LSNs,
and promotion only when acknowledged writes are protected.

## Decisions

**Replicas consume a logical replication stream.** The physical WAL remains the
local crash-recovery mechanism. Replicas consume a derived logical stream so
replication can be filtered, versioned, and kept separate from physical page/file
layout.

**Official indexes are primary-defined and replicated.** Replicas reproduce the
primary's official collection/index state deterministically. Future replica
auxiliary indexes may optimize local read workloads, but they do not affect
replicated schema state or write correctness.

**Replica auxiliary indexes are ignored on promotion.** If a replica is promoted
to primary, auxiliary indexes are discarded or ignored. They do not become
official indexes automatically.

**Replica-aware read routing is the first read-scaling optimization.** Topology-
aware clients route eligible reads to healthy/nearby replicas while respecting
bookmarks, required LSN, and freshness constraints.

**Replica reads are eventually consistent unless causality is requested.** Ordinary
replica reads may observe lag. Requests carrying a causal session token, bookmark,
or required LSN must wait briefly for a replica to reach the required contiguous
applied LSN and then fall back to an eligible node or the primary, consistent with
ADR 0031.

**Normal promotion requires the durable watermark.** Failover may promote only a
replica that has reached the commit watermark or durable LSN required by the
configured commit policy. RedDB must not silently discard acknowledged writes.

## Considered Options

- **Logical stream for replicas.** Chosen because it decouples replication from
  physical layout and supports future filtering/range movement.
- **Physical WAL replication as the primary contract.** Rejected because it tightly
  couples replicas to physical page/file layout and complicates profile evolution.
- **Replica-local indexes as official after promotion.** Rejected because promotion
  is a correctness-critical path and must not mutate schema/index authority
  implicitly.
- **Strong reads always to primary.** Rejected because it wastes replica read
  capacity; causal reads can be protected with bookmarks/LSNs.
- **Promote most-advanced available replica even if behind watermark.** Rejected
  because it can lose acknowledged writes silently.

## Consequences

- Topology metadata must expose enough health and applied-LSN information for
  replica-aware routing and bookmark eligibility.
- Replica apply must remain deterministic for official indexes and schema state.
- Auxiliary indexes need an explicit future lifecycle and cannot be part of the
  failover safety contract.
- Failover code must compare candidates against the commit policy's durable
  watermark before promotion.
