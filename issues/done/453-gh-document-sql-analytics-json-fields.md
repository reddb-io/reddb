# Add SQL document analytics with JSON field access [AFK]

GitHub issue: https://github.com/reddb-io/reddb/issues/453

## Parent

#449

## What to build

Provide the document analytics workflow from the PRD: SQL over JSON documents,
not a Mongo-style aggregation pipeline. Users should be able to filter, project,
and aggregate nested document fields using natural field access plus explicit
JSON extraction where needed.

This is unblocked because #452 is closed.

Start test-first. Add focused runtime and HTTP query tests that reproduce the
current document analytics gaps, then implement only the minimum SQL/runtime
behavior needed for those tests.

## Acceptance criteria

- [x] SQL supports document field access such as `body.level` in `SELECT`,
      `WHERE`, and `GROUP BY` where applicable.
- [x] SQL supports `json_extract(body, '$.path')` for explicit JSON path access.
- [x] SQL supports simple membership predicates such as
      `body.tags CONTAINS 'checkout'`.
- [x] Aggregates over document fields work, including `COUNT(*) AS count` and
      grouping by JSON fields.
- [x] Missing JSON paths produce stable null/no-match behavior.
- [x] Runtime and HTTP query tests cover nested fields, arrays, and aggregate
      document analytics.

## Verification

- `rtk cargo fmt`
- `rtk cargo test --test e2e_document_sql_analytics -- --nocapture`
- `rtk make check`
- JS smoke skipped: `target/debug/red` is not present in this worktree after
  `make check`.

## Guardrails

- Preserve existing #452 first-class document CRUD behavior.
- Do not introduce a Mongo-style aggregation pipeline.
- Keep syntax and behavior aligned with existing SQL parser/executor patterns.
- Do not broaden unrelated table, graph, KV, vector, or timeseries semantics.

## Blocked by

None. #452 is closed.
