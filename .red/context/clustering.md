# Clustering — RedDB Domain Glossary

Part of the [glossary map](../CONTEXT-MAP.md). Multi-writer clusters where data members own some shard/ranges and replicate others. The shared storage engine lives in [Persistence](persistence.md).

> The **Shared replication mechanics** section below is intentionally duplicated in [Primary-replica](primary-replica.md). Keep both copies in sync.

## Cluster shape & membership

- **Multi-writer cluster** — clustered RedDB deployment where multiple members may own write authority for different shards/ranges at the same time. It is still single-writer per shard/range: two members must not concurrently write the same ownership range.
- **Cluster shape** — operator-facing sizing intent for a RedDB cluster, expressed as writer count plus replica/HA posture, and optionally surfaced in product language as capacity and availability level. It describes desired topology, not the final range-placement algorithm.
- **Replication factor** — desired number of copies for each shard/range in a clustered deployment. In a multi-writer cluster this is global per shard/range, not a fixed "replicas per writer" pairing.
- **Cluster Supervisor** — RedDB-native control-plane module that manages membership, health, failover, and shard/range ownership while staying decoupled from the data-plane write path. It uses Raft or an equivalent formal consensus protocol only for the control-plane log and Supervisor leader election; user-data writes remain outside that control-plane Raft log. It can consume operator-declared desired state from external infrastructure, but RedDB remains capable of native autodetect and failover decisions.
- **Cluster member** — participant in a RedDB cluster. A data member stores data and can be a range owner for some ranges and a range replica for others; a witness member participates only in the control plane and stores no user data.
- **Cluster join** — explicit admission flow where a candidate member authenticates against seed members, verifies cluster identity, downloads global control-plane state, and only then becomes an authorized cluster member. Autodetect applies to health and topology among authorized members, not to admitting arbitrary network peers. Joining does not move user ranges; the new data member starts empty until the rebalancer schedules ownership transitions.
- **Cluster drain** — planned member-removal flow that marks a data member as draining, moves its owned and replicated ranges through normal ownership transitions, then removes it from membership once it no longer holds required data. `force remove` is reserved for dead or unrecoverable members and follows forced-ownership recovery rules.
- **Member health score** — Cluster Supervisor signal for member liveness and suitability, derived from heartbeat, replication lag, recent errors, and grace-period policy. Automatic failover uses health score rather than a single short fixed timeout to avoid flapping and false-positive promotion.
- **Voting member** — cluster member that participates in Cluster Supervisor quorum/election decisions. A resilient multi-writer cluster starts with three data members; witness members are not the recommended baseline for multi-writer clustering.
- **Control-plane consensus log** — the Cluster Supervisor's Raft-equivalent replicated log. It carries only control-plane facts — Supervisor membership changes, leader configuration, and shard/range ownership-catalog transitions — appended by the elected Supervisor leader and ordered under the current Supervisor term. User-data writes are never recorded in or gated by it (ADR 0052); durable user writes stay on the per-range commit policy and the logical replication stream. Each voting member persists durable `(term, voted_for)` vote state before acknowledging a vote so a restart cannot double-vote a term. The concrete consensus engine sits behind a small internal abstraction so follow-up slices need not choose a library or invent protocol semantics.

## Shard/range ownership

- **Shard/range ownership** — write authority for a bounded partition of collection data. In a multi-writer RedDB cluster, ownership is assigned at shard/range granularity rather than whole-collection granularity.
- **Range owner** — current writer for one shard/range. In a multi-writer cluster, write authority is a per-range role rather than a global node role.
- **Range replica** — read-only/catch-up copy of one shard/range that can be promoted to range owner if it covers the range commit watermark and wins the required ownership transition.
- **Shard key mode** — collection-level partitioning mode for shard/range ownership. Hash mode is the default for uniform distribution; ordered mode is declared when range locality and ordered scans matter more than automatic hotspot resistance.
- **Shard ownership catalog** — explicit, versioned RedDB catalog state that records shard/range bounds, current writer owner, replicas, and ownership epoch/version. It is the source of truth for range routing, failover, split/merge, and rebalancing decisions. It is special global control-plane state replicated to all data members rather than sharded like ordinary user collections. Normal writes to it are performed by the Cluster Supervisor leader; administrative recovery may force transitions under the forced-ownership rules.
- **Ownership transition** — fenced, audited change to shard/range ownership, requested either by the Cluster Supervisor or by an authorized administrative recovery command. Operators request transitions such as move, split, merge, or promote; they do not mutate shard ownership catalog rows arbitrarily.
- **Forced ownership transition** — disaster-recovery form of an ownership transition that can proceed without ordinary cluster quorum. It requires a special administrative capability, explicit operator reason, audit evidence, and an ownership epoch bump that fences any old owner still alive.

## Fencing & leases

- **Ownership fencing** — protection that prevents an old shard/range owner from accepting or replaying writes after ownership changes. Fencing applies at routing, at the local write gate, and in WAL/logical records via expected term and ownership epoch.
- **Ownership lease** — time-bounded authority for a range owner to accept durable writes, issued under the current Cluster Supervisor term and ownership epoch. If the Supervisor loses majority, owners may continue only until their valid lease expires. Production defaults should use a medium lease window rather than aggressive sub-second failover, with profile-based tuning for deployment posture.
- **Owner self-fence** — behavior where a range owner stops accepting durable writes when it loses the required control-plane quorum or ownership lease, rather than relying on clients to stop routing writes to it.
- **Self-fenced read mode** — limited mode for a self-fenced data member that may continue serving explicitly stale/read-only requests and replication catch-up, while rejecting durable writes until quorum/lease authority is restored or the member rejoins under a newer ownership epoch.

## Cross-range operations

- **Cross-range write transaction** — transaction whose write set spans shard/ranges owned by different writers. The first multi-writer cluster cut rejects these transactions rather than committing partial work or introducing two-phase commit in the ownership/failover milestone.
- **Cross-range read** — read query whose target set spans shard/ranges that may be owned by different writers. The first multi-writer cluster cut supports a simple fanout mode without a global snapshot, while explicit consistent/transactional reads require a global causal watermark or equivalent safe snapshot point.
- **Stale ownership response** — routing correction returned when a request uses an old shard/range ownership epoch. The response carries enough current epoch/owner information for clients or routers to refresh and retry; pushed topology updates are an optimization, not the correctness mechanism.

## Placement & rebalancing

- **Range striping** — distribution of collection data across multiple shard/ranges so different writers can own different parts of the data set. This is analogous to RAID striping at the cluster-data level, not block-level disk striping.
- **Range replication** — full-copy replication of each shard/range according to the cluster replication factor. The first multi-writer hot path uses full range replicas rather than parity or erasure coding.
- **Weighted placement** — shard/range placement policy that accounts for advertised node capacity such as usable disk, health, and operator weights. Expanding a node's disk changes its placement weight; data moves only through explicit rebalancing transitions.
- **Multi-signal rebalancer** — Cluster Supervisor policy that plans ownership transitions using bytes-used versus weighted capacity as the primary safety signal and read/write load as a secondary hotspot signal.
- **Split-and-move** — rebalancing transition that first divides a large or hot shard/range, then moves only the selected subrange to a different writer. Small ranges may move whole without splitting.
- **Move range cutover** — ownership transition where the old owner continues serving writes while the target first copies a physical checkpoint/snapshot of the range directory, then catches up through the logical range-indexed stream; only after catch-up does the catalog epoch move write authority to the target.
- **Range repair fallback** — cluster repair flow for a corrupted or excessively stale range replica. The first implementation cut quarantines the local range copy and rebootstrap it from a healthy owner via physical range snapshot plus logical catch-up; future repair may replace individual blocks or segments by checksum when economical.

## Cluster storage layout

- **Cluster storage profile** — RedDB posture for multi-writer clusters where data members own some shard/ranges and replicate others, using range ownership and range-indexed recovery semantics. Cluster nodes use an operational directory layout rather than embedded single-file packaging.
- **Local shard store** — per-data-member durable storage containing only the shard/ranges present on that node, whether owned or replicated. The first multi-writer cut keeps a single local `red.db`/WAL layout per node, with range identity carried in catalog and WAL/logical records so a future physical per-range layout remains possible.
- **Cluster range file layout** — operational cluster storage layout where physical files are organized by shard/range identity rather than whole collection identity, so move, repair, backup, and ownership recovery operate on the same unit as write authority. Each range is represented as a directory containing separate data, index, and append-only segment files as needed. The node still uses one physical WAL per store, with range identity in each record.
- **Range-indexed WAL** — single physical WAL stream per data member whose records carry shard/range identity and can be indexed, retained, and streamed per range. This preserves a sequential append path while supporting range replication, move-range catch-up, and per-range recovery logic.
- **Range commit watermark** — highest LSN/term for a shard/range that is known durable according to the range's configured commit policy. Failover and interrupted move-range recovery may promote only a candidate whose log covers this watermark.
- **Ephemeral-local commit** — restricted use of `local` commit policy in multi-writer clusters for collections explicitly declared ephemeral/cache-like. Durable transactional, queue, audit, config, and vault data must not silently use local-only acknowledgement when HA intent is declared.

## Shared replication mechanics

_(Duplicated in [Primary-replica](primary-replica.md) — keep in sync.)_

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
