# HTTP transport: params JSON field [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/358

Labels: enhancement

GitHub issue number: #358

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

HTTP transport accepts a `params` JSON field on the query request body:

```json
{ "sql": "SELECT * FROM users WHERE id = $1", "params": [1] }
```

Non-JSON-native types use the envelope shapes defined by the ADR (`{"$bytes": "<base64>"}`, `{"$ts": <nanos>}`, `{"$uuid": "..."}`, vectors as plain JSON arrays of numbers). Server route maps the JSON params to `Vec<Value>` and dispatches to the same binder as other transports.

## Acceptance criteria

- [ ] HTTP request body accepts `params` array; absence of `params` keeps existing `query`-only behavior.
- [ ] Every Value variant encodes/decodes correctly via JSON envelopes.
- [ ] Clear 400 errors on malformed envelopes, arity mismatch, type mismatch.
- [ ] HTTP integration test (curl-style) covering int/text/null/vector/bytes params.
- [ ] Documented in `docs/api/http.md`.

## Blocked by

- #353

## Progress (2026-05-12) — tracer slice landed

Done (HTTP transport tracer):

- `ParsedQueryRequest` (server.rs) carries an optional
  `params: Option<Vec<Value>>` alongside `query`.
- `request_body::extract_query_request` parses the `params` JSON array
  and rejects non-array shapes with a 400. Reuses
  `rpc_stdio::json_value_to_schema_value` (now `pub(crate)`) so HTTP
  and embedded stdio share one JSON→`Value` mapping.
- `handlers_query::handle_query`: when `params` is present, runs the
  shared `user_params::bind` over the parsed `QueryExpr` and dispatches
  through `runtime.execute_query_expr`. Absence preserves the legacy
  `execute_query(sql)` path so unchanged clients are unaffected.
- Tests in `handlers_query::tests`:
  - `http_query_params_select_round_trip` — int + text `$N` over
    HTTP, mirroring the stdio round-trip.
  - `http_query_params_arity_mismatch_returns_400` — typed binder
    error surfaces as HTTP 400.
  - `http_query_params_must_be_array` — non-array `params` rejected.
  - `http_query_no_params_keeps_legacy_path` — unchanged behavior.

Outstanding (rolling forward to follow-ups):

- INSERT/UPDATE/DELETE with `$N` over the binder path: blocked
  upstream — `runtime::dispatch_expr` only handles SELECT-shaped
  exprs and returns "prepared-statement execution does not support
  insert statements" for DML. Same gap exists in the embedded
  stdio path (#355's INSERT half binds correctly but is not
  executable end-to-end). Will be lifted when `execute_query_expr`
  grows DML dispatch — covered by the broader binder work, not
  HTTP-specific.
- Non-JSON-native envelopes (`{"$bytes": ...}`, `{"$ts": ...}`,
  `{"$uuid": ...}`) deferred to #356 (Value variants end-to-end)
  and #357 (wire codec). HTTP currently follows the embedded stdio
  contract: JSON number → Integer/Float, JSON string → Text, JSON
  array-of-numbers → Vector, null → Null, bool → Boolean.
- `docs/api/http.md` update deferred to the docs sweep (#374).
