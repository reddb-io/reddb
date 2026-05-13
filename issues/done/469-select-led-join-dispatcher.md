# SELECT-led JOIN dispatcher [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#466

## What to build

The parser already implements `INNER / LEFT / RIGHT / FULL / CROSS JOIN` for FROM-led statements, but a SELECT-led statement (`SELECT a.col, b.col FROM t1 a JOIN t2 b ON a.id = b.id`) is rejected at the `JOIN` token. The join executor is unchanged; only the dispatch from the SELECT arm of the parser is missing.

Extend the SELECT-led parser entry point so that after the first table reference is consumed it invokes the same join-parsing helper used by FROM-led statements.

## Acceptance criteria

- [x] `SELECT a.name, b.name FROM t a JOIN t b ON a.id = b.id` parses and executes.
- [x] Every JOIN flavor the parser already supports (`INNER`, `LEFT`, `RIGHT`, `FULL`, `CROSS`) is reachable from a SELECT-led statement.
- [x] Table aliases (`FROM t a`, `JOIN t b`) work as in FROM-led joins.
- [x] Multiple joins in one statement (`FROM t1 a JOIN t2 b ON … JOIN t3 c ON …`) parse and execute.
- [x] Existing FROM-led join tests continue to pass unchanged.
- [x] Integration tests mirror the existing FROM-led join integration tests, one per join flavor.

## Blocked by

None - can start immediately.

## Completed

- Added SELECT-led dispatch into the shared JOIN parser.
- Preserved FROM-led JOIN behavior, including legacy `RETURN ... ORDER BY ...` ordering.
- Added parser coverage for all SELECT-led JOIN flavors and multiple joins.
- Added runtime coverage for inner, outer, cross, and multiple SELECT-led table joins.

Verification:

- `rustfmt crates/reddb-server/src/runtime/query_exec.rs crates/reddb-server/src/runtime/query_exec/join.rs crates/reddb-server/src/runtime/join_filter.rs crates/reddb-server/src/storage/query/parser/table.rs crates/reddb-server/src/storage/query/parser/join.rs crates/reddb-server/src/storage/query/parser/tests.rs crates/reddb-server/src/storage/query/sql.rs crates/reddb-server/tests/runtime_query_behavior.rs`
- `CARGO_TARGET_DIR=/home/cyber/.cache/cargo-target-469 cargo test --locked -p reddb-io-server select_led -- --nocapture`
- `CARGO_TARGET_DIR=/home/cyber/.cache/cargo-target-469 cargo test --locked -p reddb-io-server join -- --nocapture`
