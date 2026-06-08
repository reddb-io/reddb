# Primary-replica — RedDB Domain Glossary

Part of the [glossary map](../CONTEXT-MAP.md). One write primary + zero-or-more read/catch-up replicas. The shared storage engine lives in [Persistence](persistence.md).

> The **Shared replication mechanics** section below is intentionally duplicated in [Clustering](clustering.md). Keep both copies in sync.

## Primary-replica profile

- **Primary-replica profile** — RedDB posture with one write primary and zero or more read/catch-up replicas; the primary decides write outcomes and replicas replay them deterministically. Small/dev presets may use a single-file `.rdb` per node, while production presets require an operational directory layout when replicas, managed backup, or WAL retention are enabled.
- **Primary-replica promotion safety** — normal failover may promote only a replica that has reached the commit watermark or durable LSN required by the configured commit policy. Promotion must not silently discard acknowledged writes.

## Replica reads

- **Replica-aware read routing** — first-line primary-replica read optimization where topology-aware clients route eligible reads to healthy/nearby replicas while respecting bookmarks, required LSN, and freshness constraints.
- **Replica freshness default** — primary-replica read contract where ordinary replica reads are eventually consistent unless the request carries a causal session token, bookmark, or required LSN. Causal reads wait briefly for a replica to reach the required contiguous applied LSN, then fall back to an eligible node or primary.
- **Replica auxiliary index** — optional future read-only index maintained locally by a replica to optimize its own read workload. Official indexes are still decided by the primary and replicated deterministically; auxiliary indexes do not affect write correctness or replicated schema state. On replica promotion, auxiliary indexes are discarded or ignored rather than becoming official indexes automatically.

## Shared replication mechanics

_(Duplicated in [Clustering](clustering.md) — keep in sync.)_

- **Topology** — canonical wire payload describing primary + replicas + each peer's region/health/lag/last-applied-LSN. Encoded by a shared encoder consumed by both RedWire HelloAck and the gRPC `Topology` RPC.
- **TopologyAdvertiser** — server-side deep module that turns the live replication state into a `Topology` payload, gated by the `cluster:topology:read` capability.
- **TopologyConsumer** — client-side deep module that parses an advertised payload, merges it against URI seed hints, and emits `ClusterMembership` with refresh hooks.
- **HealthAwareRouter** — client routing layer with EWMA RTT tracking + circuit breaker, replacing dumb modulo round-robin.
- **Any-node routing** — cluster contract where a client may send a request to any data member and the server routes or forwards it to the correct range owner when needed. Topology-aware drivers may optimize by routing directly, but correctness does not depend on client-side routing.
- **Routing hint** — protocol-level suggestion returned by RedDB, including stale-ownership responses or topology metadata, that tells a client which member is a better target for a future request. Hints optimize routing and avoid extra hops; they are not an authorization or ownership source of truth.
- **Misrouted request handling** — hybrid routing behavior for requests that arrive at a non-owner. RedDB may forward simple/idempotent operations internally, but returns redirect/routing hints for transactions, streaming, large payloads, or operations whose retry/forward semantics must stay explicit.
- **Topology refresh** — driver and router process for keeping cluster topology and ownership metadata current. Polling is the baseline, push/subscription accelerates updates, and stale-ownership responses are the mandatory correction path when cached routing is wrong.
- **Commit policy** — durability acknowledgement rule for writes, such as local, ack-n, or quorum. A cluster has a global default commit policy, and collections may declare stricter or looser overrides when their model semantics justify it.
- **Logical replication stream** — derived stream of collection/range-level changes used for replicas, range movement, bootstrap, and repair. It is derived from durable local write state rather than being the physical crash-recovery WAL itself.
- **Logical change applier** — replica-side path that consumes WAL records and applies them, bypassing the public WriteGate.
