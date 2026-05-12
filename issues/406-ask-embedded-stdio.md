# ASK via embedded stdio JSON-RPC [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/406

Labels: needs-triage

GitHub issue number: #406

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Expose ASK with citations through the embedded stdio JSON-RPC transport.

The JSON-RPC `query` method already routes `ASK` SQL today — this slice ensures all the new fields (`citations`, `sources_flat`, `validation`, `cache_hit`, `cost_usd`) are present in the JSON-RPC response. Streaming via JSON-RPC notifications is opt-in.

## Acceptance criteria

- [ ] Embedded stdio JSON-RPC `query` method returns the full new ASK schema.
- [ ] JS SDK `db.query('ASK \'...\'')` round-trips all fields.
- [ ] `STREAM` over JSON-RPC works via notification frames (or documented as not-yet if streaming is HTTP-only).
- [ ] Integration test from JS SDK against embedded engine.

## Blocked by

- #393
