# Add feedback regression pack for proven core gaps [DONE]

GitHub issue: https://github.com/reddb-io/reddb/issues/451

## Parent

#449

## What was built

Added a focused end-to-end regression pack for the proven feedback gaps that
were cheap and high-risk before broader feature work:

- Probabilistic SQL-read forms cover `SELECT CARDINALITY`, aliases, `FREQ(...)`,
  and `CONTAINS(...)`.
- `COUNT(*) AS count` returns a stable `count` column.
- KV supports quoted keys containing `:` through SQL inserts and the KV DSL.
- Time-series tags return real JSON values.
- `GRAPH PROPERTIES` preserves the user-facing `node_type`.
- The regression pack is linked from `docs/conformance/public-surface-contract-matrix.md`.

## Verification

- `rtk cargo test --test e2e_feedback_regression_pack`

## Notes

The implementation fixes only the failing regression surfaces:

- projection aliases now accept aggregate-keyword names such as `count`;
- KV DSL key parsing consumes quoted key segments as raw text and supports
  colon-qualified collection keys;
- per-node `GRAPH PROPERTIES` reads `node_type` from the unified graph entity
  rather than the lossy materialized graph label.

## Blocked by

None. #450 is closed.
