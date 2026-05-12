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

## Progress

- 2026-05-12: Slice 1 — Rust gRPC query path now accepts typed params on
  the existing backwards-compatible `Query` RPC.
  - `QueryRequest` grew optional `repeated QueryValue params = 4`; existing
    clients that send only `query`, `entity_types`, and `capabilities` keep
    the same field numbers and server path.
  - `QueryValue` is a proto3 `oneof` with variants for null, bool, int,
    float, text, bytes, vector, json, timestamp, and uuid. This mirrors the
    Rust client `params::Value` variants already used by embedded/HTTP.
  - `crates/reddb-client-connector` added `query_reply_with_params`, and
    `Reddb::query_with` now routes the `Grpc` variant through it instead of
    returning `FEATURE_DISABLED`.
  - Server-side gRPC `query` converts proto values to
    `storage::schema::Value`, parses the SQL, binds with
    `storage::query::user_params::bind`, then executes the bound
    `QueryExpr` with `execute_query_expr`. Empty params keep the legacy
    `execute_query` path.
  - `crates/reddb-client/README.md` updated so the Rust client no longer
    claims gRPC params are disabled.

  Verification:
  - `cargo check -p reddb-io-grpc-proto`
  - `cargo check -p reddb-io-server -p reddb-io-client -p reddb-io-client-connector --features reddb-io-client/grpc`
  - `cargo test -p reddb-io-client --features grpc --lib grpc_params_preserve_wire_value_variants`
  - `cargo test -p reddb-io-server --lib grpc_query_value` is blocked by a
    pre-existing compile error in
    `runtime/ai/pg_wire_ask_row_encoder.rs` tests (`temporary value dropped
    while borrowed` in two test helper calls from the prior #408 slice).

  Deferred:
  - Go driver gRPC tracer client remains open; the current Go driver has
    RedWire/HTTP parameter support but no gRPC client surface in this repo.
  - gRPC integration test covering int/text/null/vector/bytes over a real
    server remains open.
