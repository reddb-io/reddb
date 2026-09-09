# Graph identity across MVCC versions

A versioned node keeps its logical `rid` while UPDATE assigns a new physical
storage ID. Runtime MATCH/path materialization must key nodes by that logical
identity and project the snapshot-visible version's properties. Otherwise an
existing edge to the original physical version becomes disconnected on UPDATE.

## Endpoint resolution

An endpoint that already names an allowed logical node takes the existing hash
lookup path. Other numeric endpoints may name intermediate physical versions
written by older callers. Resolve only their node identity from segment storage,
cache the result for this materialization, and admit the edge only when that
logical node passed the captured snapshot and node RLS. Historical properties
are never copied or used for authorization. Edge RLS still applies separately.

Aliases span collections because item IDs are database-wide. Missing endpoints,
non-node items, deleted logical nodes and nodes denied by RLS do not create
graph nodes. This is read compatibility for retained history, not a mechanism to
reconstruct identities after their physical records have been purged.

Label-based RQL edge insertion deduplicates indexed physical versions by logical
identity, charging CALL work for candidate IDs and segment probes. Two distinct logical nodes sharing a label remain ambiguous. Numeric
edge inputs retain their existing representation and are resolved at read time.

## Cost and compatibility

For V visible nodes, E visible edges and U distinct endpoints requiring legacy
resolution, normal materialization retains its O(V + E) graph construction.
Fallback costs at most O(U × C × S) point probes for C collections with S segments
each, with O(U) query-local alias memory. It does not scan or clone historical
payloads. CALL charges each collection and segment probe, including unsuccessful
lookups, and stops when its work budget is exhausted.

There are no new writes, fsyncs, network calls, persisted fields or WAL formats.
MATCH node identity projections now remain logical across UPDATE, aligning with
the immutable `rid` contract; non-versioned IDs are unchanged. This change is
limited to the shared runtime MATCH/path materializer and RQL label admission.
Other native graph materializers require their own identity audit.

Versioning remains explicit. The existing in-place mutation semantics of
non-versioned graph collections are outside this correction. Clean reopen tests
do not establish crash/power-loss durability or competitor performance parity.

## Regression coverage

`tests/grouped/graph_analytics/e2e_graph_logical_identity.rs` exercises public
MATCH and CALL through versioned updates, savepoints, commit, deletion, rollback,
concurrent snapshots, intermediate numeric endpoints, clean reopen and node/edge
RLS. Existing graph allocation and CALL segment-pruning regressions protect the
normal materialization path.

```sh
cargo test --locked --test grouped_graph_topology -- --test-threads=1
cargo test --locked --test grouped_general_multimodel e2e_vcs_graph_mvcc_history -- --test-threads=1
```
