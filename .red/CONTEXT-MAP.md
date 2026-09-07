# RedDB Domain Glossary — Map

Reusable vocabulary for code, docs, and architecture decisions, sharded by topic. This map is the canonical entry point; each child file owns one slice of the domain language. New terms join the relevant child as they crystallize during design discussions; this remains the place to disambiguate domain language.

## Children

### Storage & deployment axis

- **[Persistence](context/persistence.md)** — the storage engine shared by every deployment profile: blob/page cache, WAL, operational manifest, file layout, checksum coverage, DDL ordering, backup/restore.
- **[Standalone](context/standalone.md)** — embedded single-file `.rdb` single-node posture, plus the offline migration path to an operational directory layout.
- **[Serverless](context/serverless.md)** — fast-boot, read-heavy, object-storage snapshot/segment-pack posture.
- **[Primary-replica](context/primary-replica.md)** — one write primary + zero-or-more read/catch-up replicas: replica read routing, freshness, promotion safety, and the shared topology/replication mechanics.
- **[Clustering](context/clustering.md)** — multi-writer clusters: shard/range ownership, Cluster Supervisor, join/drain, fencing, leases, rebalancing, cross-range semantics.

### Functional axis (orthogonal to deployment)

- **[Data model](context/data-model.md)** — how data is shaped and accessed: query & streams, catalog & discovery, keyed collection models (KV/Config/Vault), events & subscriptions, queue modes, analytics.
- **[Governance](context/governance.md)** — auth & security, telemetry channels, and compliance/evidence.

> **Shared replication terms** (Topology, TopologyAdvertiser/Consumer, HealthAwareRouter, signal plane, any-node routing, routing hints, misrouted handling, topology refresh, cooperative lease handoff, failover profile, spread rule, commit policy, logical replication stream/applier) are **intentionally duplicated** in both [Primary-replica](context/primary-replica.md) and [Clustering](context/clustering.md). When one changes, update both.

## Storage/deploy profile

- **Storage/deploy profile** — official RedDB posture that chooses the physical packaging and durability shape for the same logical database model. The baseline profiles are embedded single-file, serverless snapshot/segment, primary-replica, and multi-writer cluster. Each profile's vocabulary lives in the child files above; the shared engine underneath them is [Persistence](context/persistence.md).

## Performance gate

- **Competitive leadership gate** — per [ADR 0076](adr/0076-competitive-leadership-evidence-gate.md), the target is leadership throughout the comparable SurrealDB matrix and equivalent SQLite embedded scenarios, across foundations, product experience and multimodel capabilities. Performance cells require at least 20% lower latency or 20% higher throughput on their preselected primary metric, with equivalent correctness and guarantees and qualifying confidence intervals. Unmeasured, invalid and unfavorable cells remain visible. This is a target, not a claim about the current release. Read ADR 0076 when designing comparisons or making public performance claims.
