# MATCH ... RETURN ... LIMIT k [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#445

## What to build

The MATCH parser today rejects `MATCH (n) RETURN n LIMIT 1` with "Unexpected token after query: LIMIT". `LIMIT` works on `SELECT` but the MATCH end-of-query has no hook for it.

Extend the MATCH parser to accept an optional `LIMIT n` clause after `RETURN`. The MATCH AST gains an optional limit field. The graph executor short-circuits emission once `n` matches have been produced; the limit is applied after `WHERE` filtering and projection so the row count is what the user expects.

## Acceptance criteria

- [x] `MATCH (n) RETURN n LIMIT 1` parses and returns one row.
- [x] `MATCH (a)-[:LABEL]->(b) RETURN a, b LIMIT 10` parses and bounds the result set.
- [x] `LIMIT 0` returns zero rows.
- [x] Negative or non-integer `LIMIT` produces a clear parse error.
- [x] Existing MATCH queries without `LIMIT` continue to work unchanged.
- [x] Parser unit tests for the new AST shape; executor test asserting the result is truncated, not the input scan.

## Blocked by

None - can start immediately.

## Progress note - 2026-05-13

Implemented optional `LIMIT` on `MATCH ... RETURN`:

- `GraphQuery` now carries `limit: Option<u64>`.
- The MATCH parser accepts `LIMIT n` after the return list.
- `LIMIT 0` is valid and returns no rows.
- Negative `LIMIT` returns a `MATCH LIMIT` range error; non-integer values still produce an integer parse error.
- The materialized-graph and in-memory graph executors stop emitting rows once the limit is reached, after filtering and projection.
- Edge-expansion queries from #446 also honor the limit.

Focused verification:

- `cargo test -q -p reddb-io-server --lib test_parse_match_limit -- --test-threads=1`
- `cargo test -q -p reddb-io-server --test runtime_query_behavior match_ -- --test-threads=1`
