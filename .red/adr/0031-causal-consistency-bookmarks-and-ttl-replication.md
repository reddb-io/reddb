# Causal Consistency, Bookmarks, and TTL Replication

Status: proposed

Operator map: [Storage Profiles](../../docs/deployment/storage-profiles.md)
links this bookmark and TTL replication contract to the primary-replica recovery
boundary that depends on ADR 0030 and ADR 0032.

RedDB replicas are eventually consistent and expose an LSN, but a client has no way
to carry causality from a write to a later read, so read-your-writes across the
primary/replica boundary is not guaranteed. Separately, TTL/expiry is evaluated
against a `now` value per node (`is_expired_at(now_ns, …)`, `sweep_expired(now_ns)`
in timeseries; queue lock reclaim; cache) — queues are already primary-authoritative
("primary executes decisions; replicas replay WAL outcomes"), but any sweep that
runs on a replica's local clock is a divergence hazard. This ADR fixes the
client-facing causal-read model and the rule for how expiry replicates. It depends
on the **Commit watermark** (ADR 0030) and the WAL **term/epoch** (ADR 0032).

## Decisions

**Bookmark carry — implicit session plus explicit token.** Causality is carried two
ways at once: implicitly within a causal session (the session captures the token of
each op and attaches it to the next, giving read-your-writes + monotonic reads with
no manual threading, à la Mongo causal sessions), and explicitly as a **Bookmark**
that can be exposed and injected so causality crosses process and service
boundaries (carry it through a queue, hand it from service A to service B, à la
Neo4j bookmarks). The implicit path is the ergonomic default; the explicit path is
what lets causality span a distributed system.

**Bookmark shape — opaque (term, commit LSN).** A write returns an opaque token
carrying the term and the commit LSN. A read carrying it is guaranteed to observe
at least that write. Opacity keeps the wire format free to evolve.

**Wait semantics — bounded local wait on the contiguous applied LSN, then
fallback.** A read whose bookmark a target replica has not yet reached waits locally
up to a short deadline, then transparently falls back to a node known to be past the
bookmark (or the primary). The wait is on the replica's **contiguous applied LSN**
(the gap-free `lastClosed`, à la Neo4j), never on the gappy received frontier —
waiting on the frontier would let a read return before an earlier write in the
causal chain is applied. The bounded-wait-then-fallback shape keeps the common
sub-millisecond-lag case local and cheap while guaranteeing a lagging replica never
turns a causal read into a long stall or a hard error. This requires the routing
table to track each node's frontier and bookmark-eligibility.

**TTL/expiry replication — primary-authoritative materialization plus replica
read-time filtering.** Only the primary materializes an expiry into a delete in the
log; replicas apply that delete like any other mutation and **never** expire on
their own clock — so the persistent state can never diverge from clock skew (the
core Valkey lesson). In addition, a replica filters out, at read time, any record
already past its absolute expiry stamp, so it never serves data that is logically
expired even before the authoritative delete arrives. The read-side filter is a
view, not a state mutation, so it does not cause divergence. This generalises the
pattern queues already use; the timeseries chunk sweep and any row-level TTL must be
brought under the same primary-authoritative rule, and the read filter added on the
replica read path.

## Consequences

- The routing table (roadmap Fase 1.4) must expose per-node applied frontier and a
  bookmark-eligibility flag; a re-bootstrapping node is ineligible for bookmark
  reads (ADR-to-be / Q14 decision) even while it serves non-causal reads.
- Bookmarks couple the read path to the term + commit LSN, so they cannot ship
  before ADR 0030 (watermark) and ADR 0032 (term framing).
- The divergence detector and the TTL read-filter both assume the logical record
  carries enough to recompute expiry deterministically (absolute stamp, not a
  relative TTL re-based on a replica clock).
- Auditing every `now`-driven sweep for "does this run on a replica?" is a discrete
  work item; timeseries chunk expiry is the first known candidate.
