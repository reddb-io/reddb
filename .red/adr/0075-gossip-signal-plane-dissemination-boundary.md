# ADR 0075 — Gossip Signal Plane Dissemination Boundary

Status: accepted
Date: 2026-07-07

Resolves issue #1833 (parent #1832). Extends
[ADR 0052](0052-cluster-supervisor-control-plane-consensus.md) (Cluster
Supervisor control-plane consensus); it does not supersede it.

ADR 0052 fixed the authority boundary for Supervisor membership, leader
election, durable votes, and shard/range ownership transitions: those facts are
ordered through a Raft-equivalent control-plane log, not through heartbeat or
gossip state. Follow-up clustering work still needs a low-latency way for
already-admitted members to share observations that help routing, health
scoring, and rebalancing react faster than polling alone. This ADR defines that
non-authoritative dissemination layer and pins where it must stop.

## Decision

**RedDB adds a SWIM-style peer-to-peer signal plane among admitted cluster
members.** The signal plane is a dissemination layer, not a decision layer. It
runs only between members that have already completed the authenticated cluster
join flow, verified cluster identity, and received authorized membership state.
It carries observations that can improve local decisions and refresh timing, but
it never admits a member, elects a leader, commits a vote, transfers ownership,
or bootstraps cluster state.

**The signal-plane message vocabulary is closed.** The only message families are:

1. `LivenessObservation` — peer reachability observations, probe results, and
   incarnation counters used to detect suspect or recovered members.
2. `MemberHealthInput` — bounded health inputs such as recent errors,
   replication lag summaries, and self-fence/read-only posture.
3. `LoadMetricSample` — coarse capacity/load samples such as disk pressure,
   CPU pressure, range hotness, and write/read throughput buckets.
4. `CatalogVersionHint` — the sender's latest known ownership-catalog version,
   topology generation, or placement generation.
5. `TopologyHint` — non-authoritative peer endpoint, region/failure-domain, and
   routing-adjacent hints for already-known members.

There is no extension bag for arbitrary commands, no user-data payload, and no
control-plane log entry payload. Adding a new signal family requires a follow-up
ADR or ADR amendment because the safety boundary depends on the vocabulary being
closed.

**Some facts are never gossiped.** The signal plane must not carry membership
admission, ownership transitions, votes, or bootstrap state. Membership
admission remains the explicit authenticated cluster join flow. Ownership
transitions remain fenced, versioned catalog state proposed through the
Supervisor control-plane consensus path named by ADR 0052 and ADR 0037. Votes
remain durable per-member election state and control-plane log state. Bootstrap
state is fetched through the authenticated join/bootstrap protocol, not inferred
from peer hints.

**Signal-plane participation follows the cluster security boundary.** A node may
send or receive signal-plane traffic only after it is an authorized cluster
member. The same secured intra-cluster channel discipline used for authenticated
member-to-member traffic applies here: cluster identity is verified, peer
identity is authenticated, and unauthorized or pre-admission traffic is rejected
before its payload is interpreted. The signal plane is not exposed to clients,
seed candidates, or public discovery.

**Gossiped signals are stale by design.** A member may route, refresh, probe, or
rebalance sooner because a hint says another member is unhealthy, overloaded, or
behind a newer catalog version. That hint is only an optimization. A stale hint
may cause an extra refresh, redirect, retry, or conservative self-fence check,
but it must not make an unsafe write possible. Ownership fencing below routing
remains the correctness mechanism: writes are accepted only under the current
term/ownership epoch and local write gate, regardless of what the signal plane
last heard.

**The engine sits behind a narrow internal seam.** Implementation slices target
a small internal trait for the gossip signal plane, mirroring the
[`ControlPlaneConsensus`](../../crates/reddb-server/src/replication/control_plane.rs)
seam from ADR 0052. The seam exposes membership input from the authoritative
cluster state, emits the closed signal vocabulary above, and delivers received
signals to health/routing/rebalancing consumers. It does not expose protocol
internals, consensus semantics, or an authority API. Whether RedDB uses a SWIM
crate, a small in-house implementation, or another dissemination engine is an
implementation detail behind this seam.

## Considered Options

- **SWIM-style dissemination for signals only (chosen).** Gives fast,
  decentralized propagation for health, load, and version hints while preserving
  ADR 0052's authority boundary.
- **Gossip as membership and ownership authority.** Rejected: it contradicts ADR
  0052 and ADR 0037 by replacing quorum-ordered membership/ownership state with
  eventually consistent observations.
- **Consensus log for every health and load sample.** Rejected: it would make
  transient routing and balancing inputs compete with durable control-plane
  facts, increasing write amplification and failover latency without improving
  correctness.
- **External orchestration only.** Rejected as the sole model: operators may
  still provide desired state, but native clustering needs an internal health
  signal path among admitted members.

## Safety properties required before implementation

1. **Admission before signaling.** No signal-plane message is accepted from a
   peer that is not already admitted through authenticated cluster join.
2. **Closed vocabulary enforcement.** Encoders and decoders reject unknown
   signal families rather than treating them as opaque commands.
3. **No authority payloads.** Membership admission, ownership transitions, votes,
   and bootstrap state are absent from the signal-plane type set and wire format.
4. **Stale-safe consumers.** Health, routing, and rebalancing consumers treat
   signals as hints; they refresh or validate against authoritative state before
   taking any action that affects write authority.
5. **Fencing remains below routing.** A write can succeed only through the local
   write gate under the current term and ownership epoch, independent of gossip
   freshness.

## Consequences

- Follow-up implementation work can choose or build a SWIM-style dissemination
  engine without reopening the control-plane authority decision.
- Signal-plane tests must include stale, missing, duplicate, and out-of-order
  messages because those are normal operating conditions, not exceptional cases.
- Routing and rebalancing may consume `CatalogVersionHint` and `TopologyHint` to
  refresh faster, but stale-ownership responses and catalog refresh remain the
  mandatory correction path.
- Security review for clustering must include the signal-plane channel, but the
  channel's accepted peers are bounded by the existing authenticated cluster
  join and intra-cluster transport discipline.
