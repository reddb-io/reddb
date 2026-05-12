# HTTP transport: params JSON field [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/358

Labels: needs-triage

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
