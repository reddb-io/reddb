# EDGE insert accepts labels in from/to — DONE

GitHub: https://github.com/reddb-io/reddb/issues/420

## Root cause

`crates/reddb-server/src/runtime/impl_dml.rs` resolved the `from` and
`to` columns of `INSERT INTO <coll> EDGE (...)` through
`find_column_value_u64`, which only accepted integer literals or
decimal strings. A user-supplied node label (`'alice'`) errored with
`column 'from' expected integer, got 'alice'`. The same label
resolves fine at query time because `GRAPH NEIGHBORHOOD` /
`GRAPH TRAVERSE` go through `resolve_graph_node_id`, which consults
the materialized graph's secondary label index.

## Fix

Two surgical changes:

1. New `UnifiedStore::lookup_graph_nodes_by_label_in(collection,
   label)` in
   `crates/reddb-server/src/storage/unified/store/impl_entities.rs` —
   per-collection scoped variant of the existing
   `lookup_graph_nodes_by_label`. Same `graph_label_index` source of
   truth that `update_graph_label_index` populates on every NODE
   insert / commit, so resolution at INSERT time and at GRAPH query
   time agree.

2. New `resolve_edge_endpoint(store, collection, columns, values,
   name)` in `impl_dml.rs` replaces the `find_column_value_u64` call
   for `from` / `to`. It accepts:
   - `Value::Integer` / `Value::UnsignedInteger` → numeric id
   - `Value::Text` that parses as `u64` → numeric id (legacy)
   - `Value::Text` that does not parse → resolved via
     `lookup_graph_nodes_by_label_in(collection, &s)`:
     - 0 matches → `no graph node with label '<s>' in collection '<c>'`
     - 1 match → that entity id
     - N>1 matches → ambiguity error pointing callers at the numeric form

Mirrors `resolve_graph_node_id`'s semantics (numeric > label, error on
ambiguity) so EDGE insert and GRAPH read paths share the same mental
model.

## Acceptance criteria

- [x] `EDGE` insert accepts labels in `from`/`to`; engine resolves to
  ids via the same per-collection label index used at query time.
- [x] Numeric-id form remains supported (regression-pinned).
- [x] Clear error when label is ambiguous or absent.
- [x] Tests covering label, numeric, mixed, ambiguous, and missing
  cases — five new `#[test]` fns in
  `crates/reddb-server/tests/runtime_query_behavior.rs`:
  - `edge_insert_resolves_labels_in_from_to`
  - `edge_insert_still_accepts_numeric_ids`
  - `edge_insert_mixed_label_and_id`
  - `edge_insert_ambiguous_label_errors`
  - `edge_insert_unknown_label_errors`

## Files changed

- `crates/reddb-server/src/storage/unified/store/impl_entities.rs`
- `crates/reddb-server/src/runtime/impl_dml.rs`
- `crates/reddb-server/tests/runtime_query_behavior.rs`

## Notes for next iteration

- Driver-side surface remains unchanged: drivers pass `from`/`to`
  through as JSON params, and HTTP/redwire serialize strings as
  `Value::Text`, which now flow through the new resolution path. No
  driver code changes were needed.
- The cross-collection `lookup_graph_nodes_by_label` is still around
  for callers that legitimately need the global view; INSERT EDGE
  intentionally uses the per-collection variant to avoid resolving an
  alice-in-tales to an alice-in-asset.
- Pre-existing test failures unrelated to this slice in the same
  test binary: `config_reference_compares_stored_value_without_reparsing_sql`,
  `join_query_executes_against_real_table_rows`, and
  `secret_reference_compares_vault_value_without_reparsing_sql` —
  surface independently of this change.
