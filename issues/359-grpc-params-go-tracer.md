# gRPC parameterized query support (Go driver tracer) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/359

Labels: needs-triage

GitHub issue number: #359

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

gRPC transport carries parameterized queries end-to-end, with the Go driver as the tracer client:

```go
rows, err := db.Query(ctx, "SELECT * FROM users WHERE id = $1", 1)
```

Adds a `QueryWithParams` RPC (or extends the existing query RPC with an optional `params` field — to be confirmed in the ADR), with proto messages mirroring the wire Value enum. Server dispatches to the same binder as RedWire/HTTP/embedded.

## Acceptance criteria

- [ ] gRPC proto includes typed Value message with one variant per engine Value.
- [ ] Server handles parameterized query path identically to other transports.
- [ ] Go driver `db.Query(ctx, sql, params...)` works for int, text, null, vector, bytes.
- [ ] Backwards compatible with existing gRPC clients that send no params.
- [ ] Integration test in `drivers/go/`.

## Blocked by

- #353
