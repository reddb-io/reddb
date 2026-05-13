# Allow `count` as a column identifier in expression contexts [AFK]

Labels: enhancement, needs-triage

## AFK instruction

Implemented as a focused vertical slice. The aggregate keywords `count`, `sum`, `avg`,
`min`, and `max` can now be used as user column names where column identifiers are
expected, while aggregate function calls such as `COUNT(*)` and `SUM(count)` still parse
as functions.

## Parent

#445

## Acceptance criteria

- [x] `CREATE TABLE tw (word TEXT, count INTEGER)` parses and the column is queryable.
- [x] `INSERT INTO tw (word, count) VALUES ('wolf', 5)` parses and stores the value.
- [x] `SELECT word, SUM(count) FROM tw GROUP BY word` returns the aggregate, not null.
- [x] `SELECT count FROM tw` returns the column values.
- [x] `SELECT COUNT(*) FROM tw` continues to work as the row-count aggregate.
- [x] Same coverage exists for `sum`, `avg`, `min`, `max` as user column names.
- [x] Parser tests for the resolved column reference; integration test reproducing the user's exact query.

## Verification

- `cargo test -q -p reddb-io-server --lib aggregate_keywords_as_column_identifiers -- --test-threads=1`
- `cargo test -q -p reddb-io-server --test runtime_query_behavior aggregate_keyword -- --test-threads=1`
- `cargo test -q -p reddb-io-server --test runtime_query_behavior aggregate_function_keywords_can_all_be_user_column_names -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
- `git diff --check`
