# reddb-grpc-proto

Generated gRPC protobuf types and tonic client/server stubs for
RedDB. The `.proto` source lives in this crate's `proto/` directory
and is compiled by `tonic-prost-build` at build time.

## Audience

This crate is consumed by:

- `reddb-server` — server-side dispatch handlers.
- `reddb-client` — higher-level Rust client and CLI presentation.

You usually want one of those higher-level crates instead of
depending on `reddb-grpc-proto` directly. The crate exists so the
two sides can share generated types without one depending on the
other (which would form a dependency cycle).

## What's inside

- `proto/reddb.proto` — single source of truth for the RedDB gRPC
  surface (RedDb service, all request/reply messages).
- `src/lib.rs` — `tonic::include_proto!("reddb.v1")` re-exports
  every generated type at the crate root.
- `RedDBClient` — authenticated thin client over the generated tonic stub.

## Runtime types and compatibility

The generated stubs use the RedDB compatibility distributions of Tonic/Prost
0.14.6, with zstd 0.14.3. Rust sources and MIT licensing remain upstream; the
gRPC protocol, protobuf messages and compression formats are unchanged.

Import runtime types through the stub crate so they always share its identity:

```rust
use reddb_grpc_proto::tonic::{Request, Status, transport::Channel};
```

Existing low-level Rust integrations that separately depend on upstream
`tonic = "0.14"` must migrate these imports or alias `reddb-io-tonic` as `tonic`.
Keeping the same library name does not make the two packages' Rust types equal.
The high-level RedDB client and non-Rust gRPC consumers keep their existing wire
interfaces. See [runtime provenance](../reddb-tonic/UPSTREAM.md).

## References

- [Connection strings][conn-strings] — gRPC vs RedWire vs HTTP transports
- [ADR 0001 — RedWire][adr-0001] — companion binary protocol

[adr-0001]: ../../.red/adr/0001-redwire-tcp-protocol.md
[conn-strings]: ../../docs/clients/connection-strings.md
