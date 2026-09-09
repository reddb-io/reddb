# Context search graph expansion

Context search previously expanded a raw physical graph and hydrated neighbors
with a separate physical-ID lookup. It could return pre-UPDATE properties,
traverse RLS-denied nodes or edges, and assign the seed's collection to a
neighbor stored elsewhere.

## Read contract

`search_context` installs a statement read frame for direct API calls and inherits
an existing frame for nested calls. Direct matches and graph expansion use the
same snapshot, including transaction-local writes and savepoint visibility.

Graph expansion collects snapshot-visible logical node IDs and physical storage
locations with the graph-kind-pruned scalar visitor. It uses the shared graph
visitor for visible edges and the shared retained-version endpoint resolver.
Edge RLS applies before building adjacency. Node payloads are fetched and checked
against snapshot and node RLS when reached, outside segment locks. A denied node
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

For S segments, G candidates in graph-bearing segments, V visible nodes and E
admitted edges, preparation costs O(S + G) scanning plus O(V + E) scalar storage
and adjacency sorting. Retained physical endpoint aliases have the query-local
probe cost described in [graph identity](graph-logical-identity.md). Payload
memory is proportional to hydrated candidates and returned results; denied
candidate payloads are released after policy evaluation. Edge scans still copy
the current collection's edge payloads before visiting them outside locks.
There is no new WAL record, disk format, fsync or network round trip.

This removes eager reachable-node graph materialization before edge limits are
applied. It does not provide an indexed adjacency seek or a shared peak-memory
admission bound: the scalar node map and edge adjacency still scale with the
scoped graph. CALL work/deadline checks propagate errors from preparation,
identity resolution and traversal rather than returning partial success.

The allocation regression isolates indexed search plus expansion, with global
fallback disabled after indexing the seed. It covers growing and sealed mixed
collections, 64 unrelated vector payloads and 64 unreachable large node
properties. It measures the calling thread; it is not an all-thread memory bound
or a throughput benchmark. Global fallback scan still clones its candidate
payloads, and vector expansion's separate hydration path needs its own review.
No SQLite, Postgres, Cassandra or SurrealDB performance parity is claimed here.

Regression coverage lives in
`tests/grouped/graph_analytics/e2e_search_graph_expansion.rs` and
`tests/grouped/graph_analytics/e2e_graph_materialization_allocations.rs`.
