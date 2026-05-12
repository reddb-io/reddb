# ASK via gRPC [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/407

Labels: needs-triage

GitHub issue number: #407

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#391

## What to build

Expose ASK with citations through gRPC.

gRPC proto messages mirror the response schema (`answer`, `sources_flat`, `citations`, `validation`, etc.). Streaming via server-streaming RPC is opt-in.

Go driver and any other gRPC-based clients should round-trip the schema correctly.

## Acceptance criteria

- [ ] gRPC `Query` RPC returns the full ASK schema.
- [ ] Go driver `db.Query(ctx, 'ASK \'...\'')` works.
- [ ] Streaming via gRPC server-streaming method available.
- [ ] Proto evolution preserves backwards compatibility (optional fields, no required additions).
- [ ] Integration test from Go driver.

## Blocked by

- #393
