# Native graph reads share logical identity and RLS

The native graph materializer used physical node IDs and bypassed the node/edge
RLS gates used by MATCH. A versioned UPDATE could therefore disconnect native
paths, while a node denied by RLS remained visible through GRAPH NEIGHBORHOOD
and GRAPH PROPERTIES. Property lookup also scanned physical history separately.

## Shared read boundary

Native traversal, paths, topology algorithms and whole-graph analytics TVFs use
the same materialization pipeline as MATCH: captured snapshot, graph-kind
segment pruning, node/edge RLS, logical IDs and retained physical endpoint
aliases. Native projections apply to snapshot-visible node labels/types and edge
labels before admitting endpoints. Topology-only callers do not build property
maps that their algorithms discard.

Direct runtime graph calls install a statement read frame when no enclosing
frame exists. Calls from RQL inherit the existing frame, including transaction
snapshots and savepoint writes. No new snapshot is minted inside an existing
statement. The IAM action gates of the command/transport surfaces are unchanged.

Per-node GRAPH PROPERTIES resolves the visible, RLS-admitted node by logical ID
or label in one node pass. It returns that same version's raw node type and
properties. Numeric ID takes precedence over a coinciding label; ambiguous
labels still error. This avoids rebuilding topology or exposing an older
physical version's values.

## Cost and compatibility

The native path replaces copying all collection payloads with the existing
graph-kind visitors: O(S + G) candidate traversal for S segments and G candidates
in segments that may contain graph items, plus O(V + E) graph construction.
Retained endpoint aliases keep the query-local fallback costs documented in
[Graph identity across MVCC versions](graph-logical-identity.md). Native topology
reads skip property-map construction; per-node properties scan only nodes.
No new WAL record, fsync, persisted field or network call is introduced.

Node IDs remain logical through versioned updates. RLS-hidden nodes and edges
now disappear from native results as they do from MATCH; requesting properties
of a hidden node returns not found. Existing projection and property-column
contracts remain in effect.

## Scope and remaining work

The subsequent [search expansion correction](search-graph-expansion.md) applies
the snapshot, RLS and logical-identity boundary with scalar node locations and
lazy hydration. It removes the legacy `materialize_graph_lazy` path. Whole-graph
TVFs retain their existing collection-argument semantics. Non-versioned in-place rollback and crash-durability guarantees are
separate work.

Regressions live in `tests/grouped/graph_analytics/e2e_native_graph_identity.rs`:
versioning/savepoints/rollback/reopen, node and edge RLS, direct native API
snapshots, projections and concurrent readers. Existing MATCH, graph allocation,
VCS, SQL/MVCC and policy suites remain validation gates. Allocation regressions
exercise native topology and properties alongside MATCH/CALL in a mixed
collection with 64 unrelated 64-KiB vector payloads, both growing and sealed.
