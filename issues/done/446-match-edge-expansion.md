# Multi-node MATCH: edge expansion + label filter [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#445

## What to build

Make `MATCH (a)-[:LABEL]->(b) RETURN a, b` actually traverse edges. Today the executor emits the union of single-node matches per pattern node and discards the edge label, so a 2-node pattern returns the cross-product of all nodes that match each side.

Extend the graph match executor's multi-node code path so that an edge pattern between two node aliases is honored: only pairs `(a, b)` connected by an edge whose label matches the pattern (when one is specified) are emitted. Direction (`->`, `<-`, `-`) is respected. Variable-length hops stay deferred unless the existing AST already encodes them. Single-node fast path stays untouched. The projection layer (which already works) consumes the new matched edges and exposes `a`, `b`, and optionally the edge alias `r` if present.

## Acceptance criteria

- [x] `MATCH (a)-[:LABEL]->(b) RETURN a, b` returns only pairs connected by an edge whose label is `LABEL`.
- [x] Direction is honored: `->`, `<-`, and `-` produce the documented row sets.
- [x] Omitting the label (`MATCH (a)-[]->(b) RETURN a, b`) returns all directly-connected pairs.
- [x] Projection per matched edge exposes the edge alias (`r`) and properties via `RETURN r`, `RETURN r.property`.
- [x] `WHERE` clauses on the matched pattern continue to compose.
- [x] Regression test using a small fixture (<= 5 nodes, <= 3 labeled edges) where the answer can be enumerated by hand.
- [x] The code comment in the unified executor that currently states "Multi-node patterns (edges) are still emitted as node-only matches" is removed.

## Blocked by

None - can start immediately.

## Progress note - 2026-05-13

Implemented the runtime materialized-graph `MATCH` path through the same edge-expansion matcher used by the in-memory `UnifiedExecutor`.

- `MATCH (a)-[:likes]->(b)` now follows outgoing edges and filters by label.
- `MATCH (a)<-[:likes]-(b)` follows incoming edges.
- `MATCH (a)-[:likes]-(b)` unions incoming and outgoing direct neighbors.
- `MATCH (a)-[]->(b)` returns all outgoing directly connected pairs regardless of label.
- Edge aliases are materialized for `RETURN r` and edge properties such as `RETURN r.label`, `RETURN r.source`, and `RETURN r.target`.
- `WHERE` filtering remains applied after pattern expansion and before projection.

Focused verification:

- `cargo test -q -p reddb-io-server --test runtime_query_behavior match_ -- --test-threads=1`
