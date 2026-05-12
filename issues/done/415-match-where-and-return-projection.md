# MATCH WHERE ignored and RETURN n.foo projects empty {} — DONE

GitHub: https://github.com/reddb-io/reddb/issues/415

## Root cause

The runtime entry point for `QueryExpr::Graph` in
`crates/reddb-server/src/runtime/impl_core.rs` materialises a fresh
`GraphStore` per request and calls
`UnifiedExecutor::execute_on_with_node_properties(&graph, &expr, …)`.
That path dispatched to `exec_graph_on(graph, query)` which only
collected nodes matching the pattern's inline `{key: val}` filters —
it ignored `query.filter` (the `WHERE` clause) and `query.return_`
(the `RETURN` projection) entirely. Every match was emitted as a
node-only `UnifiedRecord` with empty `values`, so HTTP/JSON callers
saw every row pass through and every row as `{}`.

A second bug surfaced as soon as filtering was wired up: the shared
SQL `WHERE` parser emits `n.label` as
`FieldRef::TableColumn { table: "n", column: "label" }`, but the
unified executor's `get_field_value` returned `None` for any
`TableColumn` in graph context — so even after `exec_graph_on`
started evaluating `query.filter`, every predicate evaluated to
`false`.

## Fix

Three localised changes in
`crates/reddb-server/src/storage/query/unified/executor.rs`:

1. `exec_graph_on` now builds `PatternMatch` records per single-node
   pattern, applies `effective_graph_filter`, and runs the matches
   through `project_match`. Edge-extension (multi-node `MATCH` with
   `-[r]->` chains) is still not supported when running through this
   `&GraphStore` entry point — `find_matching_nodes` / `extend_matches`
   live on `self.graph` — but it never worked there either, and
   #415's acceptance criteria are scoped to single-node patterns.

2. `project_match` now treats `Projection::Field(FieldRef::NodeId
   { alias }, None)` (i.e. `RETURN n`) as a whole-entity projection:
   the matched node's `id` / `label` / `node_type` plus every entry
   in `properties` get flattened into the record under `n.<key>`.
   With an explicit alias (`RETURN n AS m`) the existing
   single-field-into-`m` path is preserved.

3. `get_field_value` now resolves `FieldRef::TableColumn { table,
   column }` against the matched node/edge aliases when the table
   segment names an alias in scope. This lets the standard
   `parse_filter`-derived predicates address node properties without
   forcing the WHERE parser to know it's inside a MATCH.

## Tests

Three regression tests in
`crates/reddb-server/tests/runtime_query_behavior.rs`:

- `match_where_filters_nodes_by_label_property` — pins
  `MATCH (n) WHERE n.label = 'X' RETURN n.name` to exactly one row.
- `match_return_property_projects_actual_values` — pins per-property
  projection (`RETURN n.name`) returning real text values.
- `match_return_whole_node_surfaces_property_bag` — pins `RETURN n`
  surfacing user properties as `n.<key>` fields.

All three previously panicked; all three pass after the fix.
Suite-level `cargo test -p reddb-io-server storage::query::unified`
remains green (25 passed).

## Notes for next iteration

Multi-node MATCH (`(a)-[r]->(b)`) against the runtime's materialised
graph still doesn't filter or project — `match_pattern` /
`extend_matches` use `self.graph`, which is the empty Arc the
`execute_on_*` helpers construct. Wiring those into the foreign
`graph` is the natural follow-up if a future ticket needs edge
patterns at the SQL surface.
