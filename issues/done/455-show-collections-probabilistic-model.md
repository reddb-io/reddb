# `SHOW COLLECTIONS` reports HLL / SKETCH / FILTER under correct model [AFK]

Labels: bug, needs-triage

## Result

Implemented. Probabilistic collection creates now register catalog contracts with explicit models, and `SHOW COLLECTIONS` renders `hll`, `sketch`, and `filter` instead of falling back to table-like metadata.

## Acceptance criteria

- [x] `CREATE HLL h` followed by `SHOW COLLECTIONS` reports `model: 'hll'` for `h`.
- [x] Same for `sketch` and `filter`.
- [x] Existing model values for table, graph, vector, queue, kv, timeseries are unchanged.
- [x] Regression test creates one of each kind and asserts the reported model.

## Verification

- `cargo test -q -p reddb-io-server --test runtime_query_behavior show_collections_reports_declared_models_for_probabilistic_collections -- --test-threads=1`
- `cargo test -q -p reddb-io-server --test runtime_query_behavior create_vector_declares_dimension_and_metric -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
- `git diff --check`
