# Context search graph expansion

Context search previously expanded a raw physical graph and hydrated neighbors
with a separate physical-ID lookup. It could return pre-UPDATE properties,
traverse RLS-denied nodes or edges, and assign the seed's collection to a
neighbor stored elsewhere.

## Read contract

`search_context` installs a statement read frame for direct API calls and inherits
an existing frame for nested calls. Direct matches and graph expansion use the
same snapshot, including transaction-local writes and savepoint visibility.

Graph expansion probes a maintained per-segment index for the reached logical
node and all its retained physical versions. Incident-edge probes include those
physical aliases and the logical ID; endpoint resolution requires a visible
logical node in scope. Candidates are checked against the captured snapshot.
Incident-edge streams merge in physical-ID/requested-collection order with
32 IDs per active source/key stream. Deduplication retains only the last visible
physical ID: invisible copies allow later visible copies, while the first visible
copy takes precedence before edge RLS, including when denied. Final logical
neighbor ordering is unchanged. Edge RLS applies before ranked candidate selection. Competitive destinations
with node RLS are hydrated and authorized outside segment locks before occupying
a slot. Unrestricted node payloads stay lazy until final selection. Only the
first authorized edges in logical endpoint/edge order remain in the query-local
adjacency cache. A denied node
cannot become a result or a traversal bridge. Endpoint identity resolution alone
never authorizes traversal.

The requested collection scope applies to seeds, edges and destination nodes.
Each result keeps its actual destination collection and visible physical entity
payload. Graph traversal and deduplication use logical node identity;
`GraphTraversal.source_id` identifies the logical source. Context cross-reference
expansion also checks the target's actual collection, snapshot and RLS before it
can contribute a graph seed. Cross-references to superseded physical versions
are skipped; historical cross-reference identity resolution is separate work.

## Expansion limits and ordering

The existing depth cap of three remains. Zero depth or zero `graph_max_edges`
disables graph expansion. The edge limit applies per expanded node, per source,
to visible, policy-admitted adjacent edges in both directions. Visited endpoints
and parallel edges still consume this edge allowance. Hidden endpoints do not.
Neighbors are ordered by logical endpoint then logical edge ID, making limited
expansion independent of hash iteration. Self-loops are included once.

Seeds are processed by descending score, then logical ID. Traversal continues
through an already scored node. Graph discoveries retain the strongest score
found across sources, with the existing 0.7 decay per hop. Direct discoveries
keep their existing provenance. Minimum score gates result insertion; the
visited set prevents cycles. Existing per-bucket result limits remain unchanged.

## Cost and limits of this slice

Each segment maintains B-tree sets of `(logical node ID, physical node ID)` and
`(numeric endpoint, physical edge ID)`. Both endpoints are indexed; self-loops
produce one pair. The index retains superseded MVCC versions until physical
reclamation. Insert, mixed bulk insert and consolidation adoption maintain it;
sealing preserves it. Existing recovery insertion paths reconstruct it from
entities. Property-only and MVCC metadata updates keep the keys. Structural
replacement or unrestricted mutable access invalidates the derived index; the
next probe rebuilds it under the segment lock with cooperative work checks. An
interrupted rebuild is discarded and never publishes partial candidates.

Cost sketch: each key probes scoped collections/segments, with O(log N + K)
work in a graph-bearing segment containing N pairs and K matching candidates.
This replaces the per-query whole-graph scan and map. Reads still pay for
collection/segment probes, retained aliases and incident degree; reached
adjacency uses a bounded ordered selection for the edge allowance. Query-local identity/hydration caches grow with probed identities, including
policy-denied nodes, not unrelated graph entries. The adjacency cache retains
only the policy-admitted edge allowance per expanded node. Bounded candidate selection and ordered cursor buffers remain temporary
per-node preparation; physical-edge deduplication uses one last-visible ID;
[their credits are reused](search-graph-candidate-cursor.md) by later expansions. The edge merge adds O(D log(R + 1)) heap work for D raw occurrences
and R active source/key streams, with O(S + 32R) cursor metadata across S sources
and a fixed 256-entry output buffer. A cold rebuild after invalidation costs O(M log N)
for M resident entities and N graph pairs. Ordinary maintained writes add
O(log N) work per pair. Accounting estimates 64 bytes per resident pair for keys,
B-tree headers and occupancy slack; this is not a hard memory admission bound.
There is no new WAL record, disk format, fsync or network round trip.

CALL work/deadline checks cover collection/segment probes, key seeks, physical
candidates, rebuilds, identity resolution and traversal. They propagate errors
instead of returning partial success. Lock waits are not individually interruptible. Candidate ordering uses bounded
B-tree operations between cooperative candidate checks. [Graph expansion admission](search-graph-memory-admission.md) now
reserves query-owned candidates, caches, graph payloads and traversal state;
precise peak-memory admission across the full search pipeline remains separate work.

Allocation regressions isolate indexed search plus expansion, with global
fallback disabled after indexing the seed. They cover growing and sealed mixed
collections, unrelated vector payloads, unreachable large node properties and
4,096 unreachable nodes plus 4,096 unreachable edges. The last case measures the
first read after insertion/sealing, without a graph-query warmup, and requires
less than 512 KiB of calling-thread allocations. This measures cumulative
allocations, not peak resident memory, all-thread memory or throughput. Global
fallback scan still clones its candidate payloads, and exact vector scoring's
payload batches remain separate work; its [segment cursor](vector-scan-cursor.md)
now avoids the per-query ID list. Context vector expansion now
[reuses its authorized selected payloads](search-vector-expansion.md). No SQLite, Postgres, Cassandra or
SurrealDB performance parity is claimed here.

A standalone public `search_context_input` allocation probe used the same query,
returned the same two nodes, disabled global fallback/reindex after seed indexing,
and measured the first query after unrelated bulk insertion:

| Unreachable nodes / edges | Parent whole-graph preparation | Indexed preparation |
|---|---:|---:|
| 0 / 0 | 10,929 bytes | 10,175 bytes |
| 4,096 / 4,096 | 7,278,621 bytes | 10,175 bytes |

These are calling-thread allocated bytes from local debug builds, not timing or
throughput measurements. The maintained index moves work and memory into writes;
write-throughput and high-degree workloads still need separate measurements.
The work-budget regression also expands the connected pair with a 128-unit
budget despite 8,192 unrelated graph entities, for growing and sealed segments.

Regression coverage lives in
`tests/grouped/graph_analytics/e2e_search_graph_expansion.rs` and
`tests/grouped/graph_analytics/e2e_graph_materialization_allocations.rs`.
