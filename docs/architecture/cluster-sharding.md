# Cluster Sharding

This page documents RedDB's cluster sharding contract as it exists in the
cluster control-plane code today. It is intentionally narrower than a production
claim: the Docker and Helm `cluster` profiles provide stable pod identity,
discovery, and `REDDB_STORAGE_PRESET=cluster`, while the full distributed
runtime that wires this model into every request path is still maturing.

The implementation lives under `crates/reddb-server/src/cluster/`, especially:

- `ownership.rs` for collection range ownership, epochs, catalog versions, and
  the per-range public write gate.
- `slot.rs` for the hash-slot primitive: the fixed slot count, the
  `shard_key -> hash -> slot` function, and the slot-to-range-key encoding.
- `routing.rs` for any-node routing, forwarding, and redirect hints.
- `cross_range.rs` for first-cut cross-range transaction and read behavior.
- `placement.rs` and `move_range.rs` for planning rebalancing, hotspot relief,
  range split, copy, catch-up, and cutover.

## Current Status

| Capability | Status |
|---|---|
| Range ownership catalog | Landed as a pure control-plane model |
| Hash and ordered collection sharding modes | Landed in the catalog model |
| Fixed hash-slot primitive (16,384 slots) | Landed in `slot.rs` |
| Any-node routing decisions | Landed as pure routing decisions |
| Public write fencing by owner epoch | Landed in the catalog model |
| Cross-range write transaction guardrails | Landed as pure planning decisions |
| Explicit, budgeted best-effort read fanout plan | Landed as pure planning decisions |
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

Logical partitioning is a separate schema/query concept documented in
[Partitioning](../query/partitioning.md). Cluster shard/range ownership is a
physical placement and failover concept; the Cluster Supervisor owns live
placement state, not ordinary table DDL.

## Placement Authority And Range Owner

The operating model uses two accepted authority terms:

- **Placement Authority** is the control-plane authority for the ownership
  catalog slice of one collection group. It decides and publishes range owners,
  hot replicas, archive replicas, ownership epochs, catalog versions, and
  placement transitions for that collection group.
- **Range Owner** is the data-plane writer for one range at one ownership epoch.
  It accepts durable writes only while the current catalog assigns that exact
  `(collection, range_id, ownership_epoch)` to it.

A collection group is the Placement Authority scope. Small related collections
may share one collection group; a large collection may use its own collection
group as its operational domain. Move-range, split, rebalance, and promote
requests are planned within that collection group scope, then executed as
per-range, per-epoch handoffs. A Placement Authority is never a data file and is
not the writer for the user records in the ranges it governs.

Production clustered ranges use triple range replication in the placement model:

- the current Range Owner;
- at least one hot mirror that can become Range Owner after it proves it covers
  the range commit watermark and wins the ownership transition;
- an archive replica optimized for restore, compression, or long-retention
  recovery.

Archive replicas cannot promote directly. They are recovery sources, not hot
write-ready mirrors. Before an archive-sourced copy can become owner, recovery
must restore the bytes, validate checksums, validate the covered commit
watermark, and record the evidence. If the archive does not cover the latest
committed watermark, forced recovery must expose the resulting skipped-data or
RPO evidence instead of silently publishing a new owner.

Routers, drivers, and data members may hold routing caches or serving-graph
projections produced from Placement Authority state. Those caches are stale by
default: a move, split, failover, or catalog-version advance can invalidate
them. Stale topology is corrected by redirect/routing hints and topology
refresh, but those are repair and latency mechanisms, not write authority.
Correctness comes from owner-side epoch fencing: every public write still lands
on the Range Owner, and the Range Owner rejects the write if its expected
ownership epoch is no longer current.

Hot mirror failover follows the same boundary. The Placement Authority publishes
an ownership transition with a bumped epoch only after the chosen hot mirror has
the required range data evidence, especially commit-watermark coverage. The new
Range Owner then accepts writes under the new epoch; the old owner is fenced
from public writes even if some client or router still has stale topology.

This preserves the control-plane/data-plane split: control-plane consensus and
Placement Authority logs carry membership, ownership, placement, and recovery
decisions. User-data writes do not enter that log and are not authorized by a
routing cache. Durable user writes remain on the Range Owner data path, guarded
by the range commit policy, replication stream, and owner-side epoch fence.

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

### Hash Slots

In `Hash` mode the indirection above is concrete: a shard key is first mapped to
a fixed *hash slot*, and slots are what the catalog ranges actually cover.

- The slot count is a cluster-format constant — `PRODUCTION_HASH_SLOT_COUNT =
  16_384` — not `node_count`. Adding or removing members does not remap the
  keyspace; it only moves which member owns a slot span.
- `hash_shard_key_to_slot` hashes the shard-key bytes with BLAKE3, takes the
  first 8 bytes big-endian, and reduces modulo the slot count. The mapping is
  stable and deterministic for a given key.
- Each slot encodes a big-endian `range_key`, so a contiguous slot span folds
  directly into the catalog's existing half-open `[lower, upper)` `RangeBounds`.
  A `RangeOwnership` entry in hash mode therefore represents a contiguous span
  of slots, and the catalog records which owner holds that span.

So a slot is the *logical* hash bucket and a range is still the *cataloged*
ownership unit. Point routing is formulaic — `shard key -> hash -> slot -> range
-> owner` — while ownership remains versioned catalog state that the supervisor
splits and moves. The user-facing routing example reads, in full hash mode:

```text
tenant_id='acme' -> hash -> slot 9381 -> range [8192, 12288) -> shard group -> owner
```

This slot layer is shipped as a pure primitive in `slot.rs` and consumed by the
ownership and topology models. It is not yet wired into a finished cluster
serving path, and there is still no public `SHARD BY` DDL that lets a user
declare the shard key — see [Not Yet Promised](#not-yet-promised). The full
slot-map and cross-shard contract is specified in
[ADR 0055](../../.red/adr/0055-cluster-slot-map-and-cross-shard-operations.md)
(status: proposed).

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
| Best-effort multi-key read | Requires explicit fanout when it crosses owners, enforces owner/range/target budgets, and is not a global snapshot |
| Consistent multi-key read | Requires a global safe watermark covering every touched range |
| Distributed SQL plan, shard execution, and merge | Roadmap |

This means cross-writer ACID is not part of the current cluster contract. If an
application needs a business workflow that spans writers, the first expected
pattern is a saga or another explicit compensating workflow above RedDB, not an
implicit distributed transaction in the engine.

Best-effort fanout is deliberately not transparent magic. The planning model
admits same-owner reads by default, but a read that would scatter across owners
must opt into fanout and fit within caller-supplied owner, range, and target
budgets. The plan also carries trace metadata — target count, owner count, range
count, and whether partial results are allowed — so the transport/executor layer
can expose scatter behavior instead of hiding it behind a normal-looking query.

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

- A public `SHARD BY` DDL surface for declaring the shard key. The hash-slot and
  ownership primitives exist, but parsing/wiring `SHARD BY` is not implemented.
- Full multi-writer serving in the container/Helm `cluster` profile.
- Distributed SQL planning with shard-local subplans and coordinator merge.
- Automatic cluster-wide result cache invalidation.
- Transparent ACID transactions across multiple range owners.
- Global secondary indexes or cross-shard uniqueness without compensating
  writes.
