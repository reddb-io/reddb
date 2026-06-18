# Cluster Sharding

This page documents RedDB's cluster sharding contract as it exists in the
cluster control-plane code today. It is intentionally narrower than a production
claim: the Docker and Helm `cluster` profiles provide stable pod identity,
discovery, and `REDDB_STORAGE_PRESET=cluster`, while the full distributed
runtime that wires this model into every request path is still maturing.

The implementation lives under `crates/reddb-server/src/cluster/`, especially:

- `ownership.rs` for collection range ownership, epochs, catalog versions, and
  the per-range public write gate.
- `routing.rs` for any-node routing, forwarding, and redirect hints.
- `cross_range.rs` for first-cut cross-range transaction and read behavior.
- `placement.rs` and `move_range.rs` for planning rebalancing, hotspot relief,
  range split, copy, catch-up, and cutover.

## Current Status

| Capability | Status |
|---|---|
| Range ownership catalog | Landed as a pure control-plane model |
| Hash and ordered collection sharding modes | Landed in the catalog model |
| Any-node routing decisions | Landed as pure routing decisions |
| Public write fencing by owner epoch | Landed in the catalog model |
| Cross-range write transaction guardrails | Landed as pure planning decisions |
| Best-effort read fanout plan | Landed as pure planning decisions |
| Consistent cross-range read watermark contract | Landed as pure planning decisions |
| Weighted placement and hotspot-aware rebalance plan | Landed as pure planning decisions |
| Full transport/storage integration for cluster mode | In progress |
| Distributed SQL planner and merge executor | Roadmap |
| Automatic cluster-wide result cache invalidation | Roadmap |

## Unit Of Sharding

RedDB shards a collection into owned ranges. A range is the unit that moves,
fails over, and accepts writes.

Each catalog entry records:

- `collection` and `range_id`
- `shard_key_mode`
- half-open `RangeBounds` as `[lower, upper)`
- current writer `owner`
- read/catch-up `replicas`
- `ownership_epoch`
- monotonic `catalog_version`
- placement metadata such as replication factor and attributes

The ownership catalog is global control-plane state. Every data member holds a
full replica of it and routes locally from that catalog. The catalog is not
itself sharded by the user-data sharding rules it describes.

## Hash Vs Ordered Ranges

There are two collection-level sharding modes:

| Mode | Bounds mean | Default | Good fit | Tradeoff |
|---|---|---:|---|---|
| `Hash` | Hash-token byte ranges | Yes | Even distribution and hotspot resistance | Ordered scans and range locality usually fan out |
| `Ordered` | Ordered shard-key byte ranges | No | Range locality and ordered scans | Sequential keys can create hot ranges |

This is the important bit: RedDB always stores shard ownership as ranges. In
`Hash` mode those ranges are over hash-token space. In `Ordered` mode those
ranges are over the ordered shard-key bytes. So the answer to "do we have range
and hash sharding?" is yes, but not as two unrelated systems: hash sharding is
implemented as ranges over a hashed keyspace.

Hash mode is also not `hash(key) % node_count` as the long-term cluster
contract. The intended contract is:

1. Hash the collection's shard key into a stable token.
2. Look up the token in the range ownership catalog.
3. Route to the owner of that token range.

That indirection is what avoids a full reshuffle every time the node count
changes. Adding capacity changes placement weights and creates move/split plans;
it does not change every key's owner by changing a modulo divisor. Consistent
hashing can still be useful for initial token/range layout, but the source of
truth is the versioned ownership catalog.

A collection cannot mix the two modes. The first range, or an explicit
collection declaration, fixes the collection's `ShardKeyMode`. Later catalog
updates that try to add a range of the other mode are rejected with
`ShardKeyModeMismatch`.

## Who Chooses The Mode

The choice is per collection, not per request.

By default, RedDB should place new clustered collections in `Hash` mode because
it gives the safest general-purpose distribution. A user or operator opts a
collection into `Ordered` mode only when locality matters more than uniform
spread, for example ordered scans over a tenant/time shard key.

Normal users should not have to hand-author every range boundary. Range
boundaries and ownership are control-plane state. The user chooses the
collection-level intent, then the cluster supervisor and placement planner
materialize ranges, split large or hot ranges, and move ownership through
explicit transitions. Low-level tests and administrative tooling may establish
or update ranges directly, but that is not the intended application workflow.

## Application And Operator Contract

From the application point of view, a clustered request names a collection and a
key. It does not pick a node, range id, modulo bucket, or owner. Routing derives
the owner from the catalog:

```text
(collection, key) -> shard key bytes -> token or ordered key -> owned range -> owner
```

The contract is intentionally split this way:

| Actor | Chooses | Does not choose |
|---|---|---|
| Application | Collection, key, and query shape | Node, range id, owner epoch, or range boundary |
| Collection declaration | Shard key and `Hash` or `Ordered` intent | Per-request placement |
| Cluster supervisor / placement planner | Range count, split points, replicas, and owner moves | Application semantics |
| Admin tooling / tests | Explicit catalog entries for drills and repair | Normal request routing |

Conceptually:

```text
users:  shard key = user_id, mode = Hash     # default, uniform distribution
events: shard key = tenant_id/time, mode = Ordered  # opt-in for locality
```

The exact public DDL/config syntax for declaring that intent belongs to the
runtime integration work. Until then, code and docs should describe the model as
a catalog contract, not as a finished user-facing cluster DDL surface.

## Routing

Routing starts by resolving `(collection, key)` to exactly one half-open range in
the local ownership catalog.

If the local node owns that range, it executes locally. If it does not, the node
returns one of two correct decisions:

- Forward a safe single-key point operation to the current owner when forwarding
  is enabled and the payload is within budget.
- Redirect the caller with a routing hint containing owner, range, epoch, and
  catalog version.

Transactions, streams/cursors, explicitly unsafe operations, and oversized
payloads are redirected instead of hidden-forwarded. Those operations need to be
opened directly on the owner because their correctness depends on where the
session or stream runs.

All writes still pass the owner-side public write gate. Forwarding never bypasses
fencing: the owner checks that it is still the owner and that the caller's epoch
matches the current range epoch.

## Cross-Range Operations

The current contract is deliberately conservative.

| Operation | Behavior |
|---|---|
| Single-key point read/write | Route to one range owner; may be forwarded from a non-owner when safe |
| Write transaction touching one writer's ranges | Admitted to that writer, even if it touches several ranges owned by that writer |
| Write transaction touching multiple writers | Rejected as unsupported; there is no hidden two-phase commit |
| Best-effort multi-key read | Planned as one read leg per owner; not a global snapshot |
| Consistent multi-key read | Requires a global safe watermark covering every touched range |
| Distributed SQL plan, shard execution, and merge | Roadmap |

This means cross-writer ACID is not part of the current cluster contract. If an
application needs a business workflow that spans writers, the first expected
pattern is a saga or another explicit compensating workflow above RedDB, not an
implicit distributed transaction in the engine.

## Caching

There is no automatic cluster-wide result cache or cross-range cache
invalidation layer in the current cluster runtime.

Safe caching rules for future cluster work are:

- Point caches must include enough ownership context to avoid serving stale
  owner data after a move, such as range id plus ownership epoch or catalog
  version.
- Cross-range aggregate caches must be keyed by query shape plus topology
  generation and per-range safe watermarks.
- A range move, split, owner epoch change, or catalog version advance must be
  enough to invalidate cached routing or cached aggregate answers.
- Best-effort read fanout may cache best-effort answers, but it must not label
  them as globally consistent unless every leg was pinned to a safe watermark.

Until that invalidation layer exists, docs and examples should not claim Redis
Cluster-style automatic cache coherence for clustered RedDB.

## Rebalancing And Hotspots

The placement planner uses two signals:

- Primary signal: bytes used versus weighted capacity. This protects
  availability by moving ranges away from members that are over their fair share
  of disk.
- Secondary signal: recent read/write traffic. This identifies hot ranges and
  proposes hotspot relief when a target has capacity headroom.

The planner only returns a plan. Nothing moves implicitly.

A planned move is then classified as either a whole-range move or a
split-and-move. Large or hot ranges are split first so only part of the keyspace
moves. Cutover follows the safe move-range flow:

1. Copy a consistent physical snapshot of the range to the target.
2. Catch up by replaying the range-indexed log to the live commit watermark.
3. Move write authority through the fenced handoff transition.
4. Bump the ownership epoch so the old owner is fenced from future public
   writes.

Interrupted moves fail safe: the target is promoted only if its persisted
catch-up position covers the range commit watermark. Otherwise the catalog stays
unchanged and the old owner remains authoritative.

## Properties

The cluster sharding model is designed around these properties:

- One writer owns each range at a time.
- Replicas can hold read/catch-up copies, but public writes route to the owner.
- Ownership epochs fence stale owners below routing.
- Catalog versions reject stale or out-of-order ownership updates.
- Ranges within a collection are half-open and non-overlapping.
- A collection is either hash-sharded or ordered-range-sharded, never both.
- Placement plans do not mutate state; moves happen through explicit
  transitions.
- Cross-writer transactions are rejected instead of partially committed.

## Not Yet Promised

Do not claim these as production behavior until the runtime work lands:

- Full multi-writer serving in the container/Helm `cluster` profile.
- Distributed SQL planning with shard-local subplans and coordinator merge.
- Automatic cluster-wide result cache invalidation.
- Transparent ACID transactions across multiple range owners.
- Global secondary indexes or cross-shard uniqueness without compensating
  writes.
