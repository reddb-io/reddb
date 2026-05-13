# SQL read forms for HLL / SKETCH / FILTER [AFK]

Labels: enhancement, needs-triage

## Result

Implemented. `SELECT CARDINALITY FROM <hll>`, `SELECT FREQ('x') FROM <sketch>`, and `SELECT CONTAINS('x') FROM <filter>` now route to the existing probabilistic stores while preserving aliases, simple filtering, limit/offset, and the command-form behavior.

## Acceptance criteria

- [x] Each read form parses against the correct collection kind and returns the expected row shape.
- [x] Wrong-kind queries produce a clear error naming the supported kind.
- [x] Multiple `FREQ(...)` calls in one SELECT against the same sketch return all values in one row.
- [x] `SELECT CONTAINS('x') FROM <filter> WHERE ...` and `AS` are respected.
- [x] Tests mirror existing command forms while going through the SQL read form.

## Verification

- `cargo test -q -p reddb-io-server --test runtime_query_behavior probabilistic_sql_read_forms_match_command_results -- --test-threads=1`
- `cargo test -q -p reddb-io-server --test runtime_query_behavior probabilistic_sql_read_forms_reject_wrong_collection_kind -- --test-threads=1`
- `cargo test -q -p reddb-io-server --test runtime_query_behavior show_collections_reports_declared_models_for_probabilistic_collections -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
- `git diff --check`
