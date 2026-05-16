# ADR 0008 — Topology advertisement security model and auth gate semantics

**Status:** Accepted
**Date:** 2026-05-06
**Supersedes:** —
**Superseded by:** —
**Related issues:**
[#164](https://github.com/reddb-io/reddb/issues/164) (PRD: server-advertised topology),
[#165](https://github.com/reddb-io/reddb/issues/165) (this ADR),
[#166](https://github.com/reddb-io/reddb/issues/166) (wire payload spec),
[#167](https://github.com/reddb-io/reddb/issues/167) (TopologyAdvertiser),
[#168](https://github.com/reddb-io/reddb/issues/168) (TopologyConsumer).

## Context

PRD #164 introduces server-advertised topology so clients learn the
primary endpoint and the replica fleet from the server they happen to
connect to first, instead of carrying a fully enumerated cluster list
in their seed configuration. The throughput motivation is replica-aware
read routing: once a client knows the replicas, it can split reads
across them rather than funnel everything at the primary.

That throughput win comes with a disclosure surface. The list of
replica endpoints, their counts, and any future capability hints we
attach to them are operationally useful to a legitimate caller and
operationally useful to an attacker probing the cluster shape. The
question this ADR settles is *who* gets to see the full topology, who
gets a redacted view, and how we make that distinction auditable
rather than implicit.

The maintainer approved four design questions during the to-prd
checkpoint that produced PRD #164:

1. **Auth gate.** A single capability `cluster:topology:read` controls
   advertisement. Default-on for authenticated principals, default-off
   for anonymous. (Approved.)
2. **Seed vs advertised topology.** Seed URI remains a hint; the
   server-advertised topology is authoritative when the two disagree.
   (Approved.)
3. **PerEndpointPool.** Ships in the same PRD as the advertiser /
   consumer pair, not deferred. (Approved.)
4. **Decomposition.** Publish PRD #164 plus seven implementation
   slices. (Approved.)

This ADR records the security posture behind answer (1) and the
schema-evolution rule that lets answer (2) hold over time. Answers
(3) and (4) are scope decisions and live in the PRD.

## Decision

### 1. `cluster:topology:read` is the only predicate that gates advertisement

Advertisement is gated by a single capability check —
`cluster:topology:read` — evaluated against the principal that opened
the connection. No other predicate gates it: no IP allowlist, no
tenant flag, no per-endpoint ACL. One capability, one check, one place
to grep.

The capability lives in the existing capability-checking
infrastructure. Granting and revoking it goes through the same
machinery operators already use for every other server-side capability,
which means topology disclosure inherits whatever audit log, whatever
policy editor, and whatever revocation latency the rest of the
capability surface already has. We are not introducing a parallel
gating mechanism for one feature.

### 2. Authenticated principals get the capability by default

The default principal template grants `cluster:topology:read`. The
typical RedDB deployment is a fleet of authenticated application
clients talking to an authenticated cluster; that is the population
that benefits most from replica routing, and forcing every operator to
hand-grant the capability before clients see any throughput win would
make the secure case the expensive case.

Default-on-for-authenticated is the explicit posture: the deployment
that already paid for authentication gets the throughput improvement
for free. Operators who want stricter posture revoke the capability
from specific principals or from the default template — see point 5.

### 3. Anonymous principals receive a `primary`-only payload

Connections that are not authenticated — the unauth probe, the
pre-handshake bootstrap, anything the server cannot tie to a principal
— do not receive replica metadata. They receive a `Topology` payload
populated only with the primary endpoint.

The primary-only payload is not an error. Unauthenticated clients
still need a working write path: the primary endpoint is what they use
to send writes (and, before they know better, reads). Returning an
empty topology or an auth error would force every legitimate
unauthenticated bootstrap into a special case. Returning the primary
covers the bootstrap without enumerating the replica fleet to a
caller we cannot identify.

### 4. Wire schema versioning policy

The canonical `Topology` struct is wrapped in a versioned envelope.
Future additions — capability hints, advertised TLS / SNI hints,
richer replica metadata, geographic affinity, anything we have not
thought of yet — go in as **new optional fields** under that envelope.

The forward-compatibility rule is binding on every future change:

- New fields are optional. A consumer that does not know the field
  ignores it.
- Old consumers parsing a newer payload never panic. Unknown fields
  are dropped, not rejected.
- A schema version bump is reserved for changes that a naive optional
  field cannot express (a removed field, a renegotiated meaning, a
  framing change). It is not the default move.

The intent is that the envelope keeps working across rolling upgrades
in both directions: an older client talking to a newer server, a
newer client talking to an older server. The exact wire bytes — field
tags, encoding, envelope shape — belong to slice #166 and are out of
scope for this ADR.

### 5. Multi-tenant disclosure trade-off

Under the default posture, every authenticated principal in a
multi-tenant deployment learns the same topology, which means
co-tenants learn each other's replica counts. We are recording this
explicitly because it is a posture choice, not an oversight.

Operators who require stricter isolation revoke
`cluster:topology:read` for tenant principals (or restructure their
default template to omit it). The advertisement for those callers
collapses to the primary-only payload defined in point 3. They lose
replica-aware routing for those tenants — that is the cost of the
stricter posture, and it is paid in throughput, not in correctness.

The trade-off is intentionally exposed at the capability level rather
than buried in a tenancy-aware advertiser. Operators see one knob,
they grep one capability, and the failure mode of the stricter posture
(slower reads through the primary) is the same failure mode as the
anonymous case — tested on the same code path.

## Consequences

**Benefits.**

- One auditable capability covers the entire disclosure surface.
  Granting, revoking, and logging happen in the existing capability
  infrastructure.
- The common deployment (authenticated clients, replica routing
  desired) gets the throughput win with zero operator action.
- The bootstrap path for unauthenticated callers still works — they
  receive the primary endpoint and can send writes — without leaking
  the replica fleet.
- Multi-tenant operators who need isolation have a documented,
  testable lever: revoke `cluster:topology:read` and the topology
  collapses to primary-only for that principal.
- Wire schema can grow new optional fields without coordinating a
  fleet-wide upgrade. Schema-version bumps are reserved for changes
  that genuinely break the optional-field contract.

**Costs.**

- Default-on-for-authenticated means co-tenants learn each other's
  replica counts unless the operator opts into the stricter posture.
  This is recorded above as the explicit trade-off.
- The forward-compatibility rule binds future contributors: every new
  field has to be optional and ignorable. Reviewers have to enforce
  that on PRs touching the topology payload.
- The primary-only payload returned to anonymous callers is a second
  shape of the same struct, which the consumer has to handle without
  branching its read-routing logic on auth state.

**Open questions.**

- Whether the capability should ever auto-revoke under suspected
  enumeration abuse (rate-limited reveal, throttled re-advertisement).
  Out of scope for this PRD; revisit if probing turns up in the wild.
- Whether the advertised topology should expose a per-replica health
  hint or leave health probing to the client. Deferred to the
  TopologyConsumer slice (#168) and the PerEndpointPool work.
- Whether the schema version itself should be advertised as a
  capability handshake field or carried in the envelope only. Slice
  #166 settles this.

## Cross-links

- PRD: [#164](https://github.com/reddb-io/reddb/issues/164)
- Wire payload spec: [#166](https://github.com/reddb-io/reddb/issues/166)
- TopologyAdvertiser (server side): [#167](https://github.com/reddb-io/reddb/issues/167)
- TopologyConsumer (client side): [#168](https://github.com/reddb-io/reddb/issues/168)
