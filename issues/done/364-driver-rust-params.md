# Rust driver: query_with(sql, &[Value]) [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/364

Labels: enhancement

GitHub issue number: #364

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#351

## What to build

Rust driver gets the new `query(sql, params)` overload, mapping language-native types to engine `Value` and serializing via the wire codec from #357.

Signature: `db.query_with(sql, &[Value])  // with IntoValue trait for ergonomic conversions`

Embedded and remote APIs both get the overload. `IntoValue` covers primitives, `Vec<f32>`, `&[u8]`, `serde_json::Value`, `chrono::DateTime`, `Uuid`.

## Acceptance criteria

- [ ] New `query(sql, params)` overload implemented.
- [ ] Original `query(sql)` signature unchanged.
- [ ] Native type mapping documented: int, float, bool, null, text, bytes, vector, json, timestamp, uuid.
- [ ] Driver-side parameter serialization tested (deep module per driver) — golden fixtures shared with other drivers.
- [ ] Integration test covering int/text/null/vector params end-to-end.
- [ ] README example updated with the parameterized form (especially vector example).

## Blocked by

- #357

## Progress (2026-05-12, slice 1 — query_with on Reddb + EmbeddedClient + HttpClient)

Tracer-bullet slice. New `Reddb::query_with(sql, &[V: IntoValue])`
plus dedicated `params::Value` enum + `IntoValue` trait. Mirrors the
Go driver's `Query(ctx, sql, params...any)` shape (#363) but with
Rust's typed `Value` instead of `any`.

Decisions:
- `Value` is a hand-rolled enum (Null, Bool, Int, Float, Text, Bytes,
  Vector, Json, Timestamp, Uuid) — same 10 variants the JS / Go
  drivers ship. Keeps the public API serde-version-agnostic, matching
  the existing `JsonValue` stance in `types.rs`.
- Two conversions: `into_json_param` (always-on, for HTTP) and
  `into_schema_value` (cfg `embedded`, for the in-process binder).
  Embedded path skips the JSON round-trip the JSON-RPC stdio handler
  pays.
- `IntoValue` blanket impl for the natural Rust types listed in the
  acceptance criteria (primitives, `Vec<f32>`, `&[u8]`,
  `serde_json::Value`, `Option<T>`). Chrono / Uuid crate types are
  intentionally NOT in the trait set — callers using those crates
  convert via `as_bytes()` / `timestamp()`, so we don't force a
  chrono / uuid version on every downstream.
- HTTP transport sends the same `{query, params}` envelope #358
  already wired server-side, with `{"$bytes":…}` / `{"$ts":…}` /
  `{"$uuid":…}` envelopes for the binary-ish variants (matches the
  JS driver's `serializeParam`).
- Embedded transport calls `parse_multi` → `user_params::bind` →
  `runtime.execute_query_expr`, the exact path `rpc_stdio` uses for
  its `query` JSON-RPC method. Zero new server-side code.
- gRPC transport returns `FEATURE_DISABLED` with a pointer to #359.
  Rust's `reddb_client::grpc` does not yet speak the params frame —
  #357's server-side leg landed on the JSON-RPC / HTTP paths but
  the tonic-side params frame is its own slice.
- Empty `params` short-circuits to the legacy `execute_query` fast
  path so the parameter-less hot path pays zero overhead.

Files:
- crates/reddb-client/src/params.rs (new, ~310 LOC, 9 unit tests)
- crates/reddb-client/src/embedded.rs (`query_with`)
- crates/reddb-client/src/http.rs (`query_with`)
- crates/reddb-client/src/lib.rs (module + re-export + `Reddb::query_with`)
- crates/reddb-client/README.md (parameterized examples + type table)
- crates/reddb-client/tests/embedded_query_with.rs (new, 7 tests)

Verification:
- `cargo check -p reddb-io-client` clean.
- `cargo check -p reddb-io-client --features http,grpc` clean.
- `cargo check -p reddb-io-client --no-default-features` clean (only
  pre-existing unused-var warnings on the cfg-gated arms).
- `cargo test -p reddb-io-client --lib` → 45 passed (36 prior + 9 new).
- `cargo test -p reddb-io-client --test embedded_query_with` → 7 passed.
- `cargo fmt --check -p reddb-io-client` clean.

Blockers / notes for next iteration (#373):
- Cross-driver golden-fixture wiring is its own slice. The deep
  `params` module here is what #373's Rust binding will conform
  against — `Value::into_json_param` is byte-stable; a future binary-
  wire encoder mirroring `drivers/go/redwire/value.go` is needed when
  the rust client grows a RedWire transport.
- gRPC params (#359) still needs the proto field + server-side route
  to land before `Reddb::Grpc` can drop the `FEATURE_DISABLED` arm.
