# SELECT decodes `Value::Json` instead of returning `<json N bytes>` placeholder [AFK]

Labels: bug, needs-triage

## AFK instruction

Implemented as a focused vertical slice. SELECT JSON presentation now decodes stored JSON
bytes through the shared presentation decoder in both HTTP-style result rendering and the
stdio/cursor result codec. Corrupt JSON falls back to a structured object with
`code = INVALID_JSON` and a hex payload.

## Parent

#445

## Acceptance criteria

- [x] `INSERT INTO ts1 (metric, value, tags, timestamp) VALUES ('cpu', 85, '{"host":"a"}', 1000)` followed by `SELECT tags FROM ts1` returns `{"host":"a"}`, not `<json 12 bytes>`.
- [x] The same fix covers `Value::Json` in any other SELECT path (table rows, projected columns, JOIN outputs).
- [x] Existing callers that rely on a string `tags` get a structured value.
- [x] Regression test: round-trip a non-trivial JSON object (nested + array) through INSERT -> SELECT and assert byte-equal recovery after a single parse.

## Notes

- This intentionally changes stdio SELECT JSON results from lossy strings to structured JSON.
- While adding the timeseries regression, DML column list parsing was restored to the broader keyword-tolerant path so `metric`, `value`, `tags`, and aggregate-keyword user columns all work together.

## Verification

- `cargo test -q -p reddb-io-server --lib select_timeseries_tags_decodes_json_payload -- --test-threads=1`
- `cargo test -q -p reddb-io-server --lib select_table_json_column_round_trips_after_single_parse -- --test-threads=1`
- `cargo test -q -p reddb-io-server --lib select_json_corruption_falls_back_to_code_and_hex -- --test-threads=1`
- `cargo test -q -p reddb-io-server --lib aggregate_keywords_as_column_identifiers -- --test-threads=1`
- `cargo test -q -p reddb-io-server --test runtime_query_behavior aggregate_keyword -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
