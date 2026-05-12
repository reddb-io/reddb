# Read API: SELECT row-projection returns empty for graph collections — DONE

GitHub: https://github.com/reddb-io/reddb/issues/414

## Root cause

`runtime_table_record_from_entity` and its four companions in
`crates/reddb-server/src/runtime/record_search.rs` only materialized
`EntityData::Row` (and `TimeSeries`), returning `None` for any other
entity kind. The SELECT row-projection scan paths use these via
`filter_map`, silently dropping graph nodes/edges, vectors, and queue
messages. Aggregates worked because `query_exec/aggregate.rs` iterates
over raw entities and reads kind-level fields without going through
the row materializer.

## Fix

Six functions in `record_search.rs` now fall back to
`runtime_any_record_from_entity[_ref]` for non-Row/TimeSeries entities:

- `runtime_table_record_lean` / `_ref`
- `runtime_table_record_from_entity` / `_ref`
- `runtime_table_record_from_entity_projected` / `_ref_projected`

Projection variants accept the full record (the outer projection layer
keeps only requested columns); correctness over micro-optimization for
the graph path which never worked.

## Tests

Added two regression tests in
`crates/reddb-server/tests/runtime_query_behavior.rs`:

- `select_star_returns_graph_nodes_inserted_into_collection` — covers
  `SELECT *` and `SELECT label, name ... WHERE label = ...`.
- `aggregate_over_graph_collection_still_works` — pins existing
  aggregate behavior so the fix didn't break the path that already
  worked.

Full reddb-io test sweep: 667 passed, 11 ignored.
