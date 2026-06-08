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

> **Shared replication terms** (Topology, TopologyAdvertiser/Consumer, HealthAwareRouter, any-node routing, routing hints, misrouted handling, topology refresh, commit policy, logical replication stream/applier) are **intentionally duplicated** in both [Primary-replica](context/primary-replica.md) and [Clustering](context/clustering.md). When one changes, update both.

## Storage/deploy profile

- **Storage/deploy profile** — official RedDB posture that chooses the physical packaging and durability shape for the same logical database model. The baseline profiles are embedded single-file, serverless snapshot/segment, primary-replica, and multi-writer cluster. Each profile's vocabulary lives in the child files above; the shared engine underneath them is [Persistence](context/persistence.md).

## Performance gate

- **Scenario-specific gate** — per ADR 0009, RedDB does not commit to "20% faster than every competitor on every scenario". Instead, it commits to winning where the unified-engine architecture structurally outperforms (typed_insert, disk_usage, cross-model queries) and to parity-or-close-gap elsewhere.
