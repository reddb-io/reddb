# Shard/range Ownership Catalog

Status: proposed

RedDB multi-writer clustering uses explicit, versioned shard/range ownership
catalog state as the source of truth for routing and failover. Each owned range
records its bounds, writer owner, replicas, and ownership epoch/version. This
chooses a MongoDB/Cockroach-style cataloged placement model over a Cassandra-style
ring where ownership is derived only from membership.

## Decisions

**Ownership is assigned at shard/range granularity, not whole-collection
granularity.** A multi-writer cluster may have different writers for different
ranges of the same collection, while preserving single-writer authority for each
range.

**The catalog is authoritative.** Hash or ordered partitioning may propose initial
placement, but live ownership, failover, split/merge, and rebalancing decisions
read and write versioned catalog state rather than recalculating ownership from
membership alone.

**Ownership changes are transitions, not arbitrary row edits.** The Cluster
Supervisor normally initiates ownership transitions, but authorized administrative
commands may also request transitions for fix and recovery workflows. Both paths
must use the same fenced, versioned, audited transition machinery.

**Forced transitions are reserved for disaster recovery.** Normal ownership
transitions require the ordinary cluster safety checks. A `FORCE` transition may
proceed without ordinary quorum only with a special administrative capability,
explicit operator reason, durable audit evidence, and an ownership epoch bump that
fences any old owner that later reappears.

**Fencing is enforced below routing.** Clients and routers must refresh stale
ownership metadata, but safety cannot depend on routing alone. The old owner must
also reject writes locally once its ownership epoch is stale, and WAL/logical
records must carry enough term/ownership epoch data for replicas and recovery to
reject divergent history.

## Considered Options

- **Deterministic token ring only.** Rejected because it makes ordered ranges,
  explicit failover epochs, operator-visible ownership, and future split/merge
  workflows harder to reason about.
- **Whole-collection ownership.** Rejected as the target model because it cannot
  scale a single large collection across multiple writers.

## Consequences

- Routing must consult ownership metadata with an epoch/version and handle stale
  routing responses.
- The Cluster Supervisor must update ownership through fenced, versioned
  transitions.
- Rebalancing, failover, and administrative recovery become catalog transitions,
  not just membership changes.
- ADR 0032's WAL term framing must be extended or paired with range ownership
  epochs so stale owners cannot produce acceptable writes for a range they no
  longer own.
